// Phase 15 — cross-component bridge for opening ToolsMode's Create
// tab with a pre-selected schema type. Used by the DADBEAR Oversight
// Page's "Set Default Norms" button, which needs to switch the user
// to the Phase 9/10 generative config flow with `dadbear_policy`
// already picked.
//
// We use a module-level singleton + a custom DOM event instead of
// extending AppContext because the bridge data is ephemeral (one-shot
// handoff) and does not belong in the app's persistent state tree.
// AppShell / ToolsMode pick up the signal by listening for the
// 'wire-node:tools-mode-preset' event and reading
// `pendingToolsModePreset` on mount.

export interface ToolsModePreset {
    schemaType: string;
    slug: string | null;
}

export const TOOLS_MODE_PRESET_EVENT = 'wire-node:tools-mode-preset';

let pending: ToolsModePreset | null = null;

/// Read + clear the pending preset. Call this from ToolsMode on
/// mount or when the 'wire-node:tools-mode-preset' event fires.
export function takeToolsModePreset(): ToolsModePreset | null {
    const snapshot = pending;
    pending = null;
    return snapshot;
}

/// Queue a preset for ToolsMode. Also fires a custom event so
/// an already-mounted ToolsMode can react immediately (otherwise
/// the preset waits until the next mount).
export function requestToolsModePreset(preset: ToolsModePreset): void {
    pending = preset;
    try {
        window.dispatchEvent(new CustomEvent(TOOLS_MODE_PRESET_EVENT, {
            detail: preset,
        }));
    } catch {
        // Non-browser environments — ignore.
    }
}
