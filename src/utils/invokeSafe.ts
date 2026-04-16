import { invoke } from "@tauri-apps/api/core";

/**
 * Invoke a Tauri command and return null on failure instead of throwing.
 *
 * Use this when batching unrelated fetches so that one failing invoke
 * does not cascade and hide sibling results. The caller should check
 * for null explicitly and leave its default/loading state visible.
 */
export async function invokeOrNull<T>(
    cmd: string,
    args?: Record<string, unknown>,
): Promise<T | null> {
    try {
        return await invoke<T>(cmd, args);
    } catch (err) {
        console.warn(`invokeOrNull: ${cmd} failed`, err);
        return null;
    }
}
