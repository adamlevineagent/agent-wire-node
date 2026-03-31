// Command Dispatch — translates named vocabulary commands to API calls
//
// The vocabulary registry maps named commands (like "archive_agent") to their
// dispatch details (type, method, path, body/query/header maps). The LLM only
// sees command names and parameter schemas. This module handles the translation
// from named commands to actual Tauri invoke calls.

import { invoke } from '@tauri-apps/api/core';

// ---------------------------------------------------------------------------
// Types (mirror Rust vocabulary.rs DispatchEntry)
// ---------------------------------------------------------------------------

export interface ParamDef {
    name: string;
    type: string;
    required: boolean;
    description?: string;
    source: string; // "args" | "context"
}

export type DispatchEntry =
    | { type: 'tauri' }
    | {
          type: 'wire_api';
          method: string;
          path: string;
          body_map?: Record<string, unknown>;
          query_map?: Record<string, string>;
          headers?: Record<string, string>;
      }
    | {
          type: 'operator_api';
          method: string;
          path: string;
          body_map?: Record<string, unknown>;
          query_map?: Record<string, string>;
      }
    | {
          type: 'navigate';
          mode: string;
          view?: string;
          props_map?: Record<string, string>;
      };

export interface RegistryEntry {
    dispatch: DispatchEntry;
    params: ParamDef[];
}

export type DispatchRegistry = Record<string, RegistryEntry>;

// ---------------------------------------------------------------------------
// Template interpolation
// ---------------------------------------------------------------------------

/**
 * Replace {{param}} placeholders in a string with values from args.
 * Returns undefined if the entire template is a single unresolved placeholder.
 */
function interpolateString(
    template: string,
    args: Record<string, unknown>,
): string | undefined {
    // Check if any placeholder has no value — track if all resolved
    let hasUnresolved = false;
    const result = template.replace(/\{\{(\w+)\}\}/g, (_match, key: string) => {
        const val = args[key];
        if (val === undefined || val === null) {
            hasUnresolved = true;
            return '';
        }
        return String(val);
    });
    // If there were unresolved placeholders and the result is empty/whitespace, return undefined
    if (hasUnresolved && result.trim() === '') return undefined;
    return result;
}

/**
 * Recursively interpolate {{param}} in a JSON value tree.
 * When a string template is exactly "{{paramName}}" (the entire value is one placeholder),
 * substitute the RAW value to preserve arrays/objects. Otherwise stringify.
 */
function interpolateValue(
    value: unknown,
    args: Record<string, unknown>,
): unknown {
    if (typeof value === 'string') {
        // Check for exact single-placeholder pattern: "{{paramName}}"
        const exactMatch = value.match(/^\{\{(\w+)\}\}$/);
        if (exactMatch) {
            const key = exactMatch[1];
            const rawVal = args[key];
            // Return the raw value (preserves arrays, objects, numbers, etc.)
            return rawVal;
        }
        // For compound templates like "prefix-{{param}}", do string interpolation
        return interpolateString(value, args);
    }
    if (Array.isArray(value)) {
        return value.map(v => interpolateValue(v, args));
    }
    if (value && typeof value === 'object') {
        const result: Record<string, unknown> = {};
        for (const [k, v] of Object.entries(value)) {
            result[k] = interpolateValue(v, args);
        }
        return result;
    }
    return value;
}

/**
 * Validate that a path has no traversal sequences or empty segments after interpolation.
 */
function validatePath(path: string): void {
    if (path.includes('..') || path.includes('://')) {
        throw new Error(`Invalid path after interpolation: ${path}`);
    }
    if (!path.startsWith('/api/v1/')) {
        throw new Error(`Path does not start with /api/v1/: ${path}`);
    }
    // Reject paths with empty segments from unresolved {{param}} placeholders
    const pathPortion = path.split('?')[0];
    if (pathPortion.includes('//')) {
        throw new Error(`Path has empty segments (unresolved placeholder?): ${path}`);
    }
}

// ---------------------------------------------------------------------------
// API request building
// ---------------------------------------------------------------------------

interface ApiRequest {
    method: string;
    path: string;
    body?: Record<string, unknown>;
    headers?: Record<string, string>;
}

/**
 * Build an API request from a dispatch entry + user-supplied args.
 * Handles path interpolation, body_map, query_map, and headers.
 */
export function buildApiRequest(
    dispatch: Extract<DispatchEntry, { type: 'wire_api' | 'operator_api' }>,
    args: Record<string, unknown>,
): ApiRequest {
    // Interpolate path
    const path = interpolateString(dispatch.path, args) ?? dispatch.path;
    validatePath(path);

    const req: ApiRequest = {
        method: dispatch.method,
        path,
    };

    // Build body from body_map
    if (dispatch.body_map) {
        const body: Record<string, unknown> = {};
        for (const [key, template] of Object.entries(dispatch.body_map)) {
            const val = interpolateValue(template, args);
            if (val !== undefined && val !== '') {
                body[key] = val;
            }
        }
        if (Object.keys(body).length > 0) {
            req.body = body;
        }
    }

    // Build query string from query_map and append to path
    if (dispatch.query_map) {
        const params = new URLSearchParams();
        for (const [key, template] of Object.entries(dispatch.query_map)) {
            const val = interpolateString(template, args);
            if (val && val.length > 0) {
                params.set(key, val);
            }
        }
        const qs = params.toString();
        if (qs) {
            req.path = `${req.path}?${qs}`;
        }
    }

    // Build headers
    if ('headers' in dispatch && dispatch.headers) {
        const headers: Record<string, string> = {};
        for (const [key, template] of Object.entries(dispatch.headers)) {
            const val = interpolateString(template, args);
            if (val && val.length > 0) {
                headers[key] = val;
            }
        }
        if (Object.keys(headers).length > 0) {
            req.headers = headers;
        }
    }

    return req;
}

// ---------------------------------------------------------------------------
// Step execution via registry
// ---------------------------------------------------------------------------

/**
 * Execute a plan step using the vocabulary registry.
 * Returns the result of the invocation.
 */
export async function executeViaRegistry(
    commandName: string,
    args: Record<string, unknown>,
    registry: DispatchRegistry,
    setMode: (mode: string) => void,
    navigateView: (mode: string, view: string, props: Record<string, unknown>) => void,
): Promise<unknown> {
    const entry = registry[commandName];
    if (!entry) {
        throw new Error(`Command "${commandName}" not found in vocabulary registry`);
    }

    const dispatch = entry.dispatch;

    switch (dispatch.type) {
        case 'tauri':
            return invoke(commandName, args);

        case 'wire_api': {
            const req = buildApiRequest(dispatch, args);
            return invoke('wire_api_call', {
                method: req.method,
                path: req.path,
                body: req.body ?? null,
                headers: req.headers ?? null,
            });
        }

        case 'operator_api': {
            const req = buildApiRequest(dispatch, args);
            return invoke('operator_api_call', {
                method: req.method,
                path: req.path,
                body: req.body ?? null,
            });
        }

        case 'navigate': {
            setMode(dispatch.mode);
            // Interpolate dynamic props if any — filter out empty/undefined values
            const props: Record<string, unknown> = {};
            if (dispatch.props_map) {
                for (const [key, template] of Object.entries(dispatch.props_map)) {
                    const val = interpolateString(template, args);
                    if (val !== undefined && val.length > 0) {
                        props[key] = val;
                    }
                }
            }
            if (Object.keys(props).length > 0 || dispatch.view) {
                navigateView(dispatch.mode, dispatch.view ?? '', props);
            }
            return { navigated: dispatch.mode };
        }

        default:
            throw new Error(`Unknown dispatch type for command "${commandName}"`);
    }
}
