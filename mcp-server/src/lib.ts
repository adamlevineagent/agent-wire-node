/**
 * Shared helpers for Wire Node pyramid MCP server and CLI.
 * Provides auth token resolution, HTTP fetching, and constants.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

// ── Constants ────────────────────────────────────────────────────────────────

export const WIRE_NODE_BASE_URL = "http://localhost:8765";
export const REQUEST_TIMEOUT_MS = 10_000;

// ── Auth Token Resolution ────────────────────────────────────────────────────

export function resolveAuthToken(): string {
  // 1. Environment variable takes priority
  const envToken = process.env.PYRAMID_AUTH_TOKEN;
  if (envToken) {
    return envToken;
  }

  // 2. Fall back to config file
  const configPath = join(
    homedir(),
    "Library",
    "Application Support",
    "wire-node",
    "pyramid_config.json"
  );

  try {
    const raw = readFileSync(configPath, "utf-8");
    const config = JSON.parse(raw);
    if (config.auth_token && typeof config.auth_token === "string") {
      return config.auth_token;
    }
  } catch {
    // File doesn't exist or isn't valid JSON — fall through
  }

  console.error(
    "[pyramid] FATAL: No auth token found. Set PYRAMID_AUTH_TOKEN env var " +
      "or create ~/Library/Application Support/wire-node/pyramid_config.json " +
      'with an "auth_token" field.'
  );
  process.exit(1);
}

// ── HTTP Helper ──────────────────────────────────────────────────────────────

export interface PyramidResponse {
  ok: boolean;
  status: number;
  data: unknown;
}

export async function pyramidFetch(
  path: string,
  options: { method?: string; body?: unknown; authToken: string }
): Promise<PyramidResponse> {
  const { method = "GET", body, authToken } = options;
  const url = `${WIRE_NODE_BASE_URL}${path}`;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const fetchOptions: RequestInit = {
      method,
      headers: {
        Authorization: `Bearer ${authToken}`,
        "Content-Type": "application/json",
      },
      signal: controller.signal,
    };

    if (body !== undefined) {
      fetchOptions.body = JSON.stringify(body);
    }

    const response = await fetch(url, fetchOptions);
    clearTimeout(timeout);

    let data: unknown;
    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      data = await response.json();
    } else {
      data = await response.text();
    }

    return { ok: response.ok, status: response.status, data };
  } catch (err: unknown) {
    clearTimeout(timeout);

    if (err instanceof Error) {
      if (err.name === "AbortError") {
        return {
          ok: false,
          status: 0,
          data: {
            error: "Request timed out after 10 seconds. Wire Node may be unresponsive.",
          },
        };
      }
      // Connection refused or other network error
      const connRefusedMsg =
        "Wire Node HTTP server is not reachable at localhost:8765.\n" +
        "This usually means:\n" +
        "  - The Wire Node app is not running, OR\n" +
        "  - The app is running but the HTTP server hasn't started yet (wait a few seconds)\n" +
        "Check that Wire Node is open and showing \"Online\" in the sidebar.";

      if (
        "cause" in err &&
        err.cause instanceof Error &&
        err.cause.message?.includes("ECONNREFUSED")
      ) {
        return {
          ok: false,
          status: 0,
          data: { error: connRefusedMsg },
        };
      }
      // Generic fetch errors (DNS, etc.)
      if (err.message?.includes("ECONNREFUSED") || err.message?.includes("fetch failed")) {
        return {
          ok: false,
          status: 0,
          data: { error: connRefusedMsg },
        };
      }
    }

    return {
      ok: false,
      status: 0,
      data: {
        error: `Network error: ${err instanceof Error ? err.message : String(err)}`,
      },
    };
  }
}
