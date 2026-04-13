/**
 * Hook for opening/closing pyramid surface popup windows from the main app.
 */

import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function usePyramidWindow() {
    /** Open a pyramid surface window. If slug is provided, shows that pyramid. Otherwise shows Grid View. */
    const openWindow = useCallback(async (slug?: string) => {
        try {
            const label = await invoke<string>('pyramid_open_window', { slug: slug ?? null });
            return label;
        } catch (e) {
            console.error('Failed to open pyramid window:', e);
            return null;
        }
    }, []);

    /** Close a specific pyramid window by its label. */
    const closeWindow = useCallback(async (label: string) => {
        try {
            await invoke('pyramid_close_window', { label });
        } catch (e) {
            console.error('Failed to close pyramid window:', e);
        }
    }, []);

    return { openWindow, closeWindow };
}
