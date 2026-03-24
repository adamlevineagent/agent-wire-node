#!/usr/bin/env node

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import {
  WIRE_NODE_BASE_URL,
  resolveAuthToken,
  pyramidFetch as pyramidFetchRaw,
  type PyramidResponse,
} from "./lib.js";

// ── Constants ────────────────────────────────────────────────────────────────

const SERVER_NAME = "wire-node-pyramid";
const SERVER_VERSION = "0.2.0";

// ── Auth ─────────────────────────────────────────────────────────────────────

const AUTH_TOKEN = resolveAuthToken();

/** Convenience wrapper that injects the resolved auth token. */
async function pyramidFetch(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<PyramidResponse> {
  return pyramidFetchRaw(path, { ...options, authToken: AUTH_TOKEN });
}

/** Unwrap a PyramidResponse into a tool result. */
function toToolResult(resp: PyramidResponse): { content: Array<{ type: "text"; text: string }> } {
  if (resp.ok) {
    return {
      content: [
        {
          type: "text" as const,
          text: typeof resp.data === "string" ? resp.data : JSON.stringify(resp.data, null, 2),
        },
      ],
    };
  }

  // Error case — always return structured JSON
  const errorPayload =
    typeof resp.data === "object" && resp.data !== null
      ? resp.data
      : { error: String(resp.data), status: resp.status };

  return {
    content: [
      {
        type: "text" as const,
        text: JSON.stringify(errorPayload, null, 2),
      },
    ],
  };
}

// ── Startup Connectivity Check ───────────────────────────────────────────────

async function checkConnectivity(): Promise<void> {
  try {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 3000);
    const resp = await fetch(`${WIRE_NODE_BASE_URL}/health`, {
      headers: { Authorization: `Bearer ${AUTH_TOKEN}` },
      signal: controller.signal,
    });
    clearTimeout(timeout);
    if (resp.ok) {
      console.error(`[pyramid-mcp] Connected to Wire Node at ${WIRE_NODE_BASE_URL}`);
    } else {
      console.error(
        `[pyramid-mcp] WARNING: Wire Node responded with status ${resp.status}. Tools will retry on each call.`
      );
    }
  } catch {
    console.error(
      `[pyramid-mcp] WARNING: Wire Node is not reachable at ${WIRE_NODE_BASE_URL}. ` +
        "The MCP server will start anyway — tools will return clear errors until Wire Node is running."
    );
  }
}

// ── MCP Server ───────────────────────────────────────────────────────────────

const server = new McpServer({
  name: SERVER_NAME,
  version: SERVER_VERSION,
});

// 1. pyramid_health
server.tool(
  "pyramid_health",
  "Check Wire Node server health, version, and connectivity status",
  {},
  async () => {
    const resp = await pyramidFetch("/health");
    return toToolResult(resp);
  }
);

// 2. pyramid_list_slugs
server.tool(
  "pyramid_list_slugs",
  "List all available pyramid slugs with their content types and metadata",
  {},
  async () => {
    const resp = await pyramidFetch("/pyramid/slugs");
    return toToolResult(resp);
  }
);

// 3. pyramid_apex
server.tool(
  "pyramid_apex",
  "Get the apex (top-level) node of a pyramid — the system overview and architecture summary",
  { slug: z.string().describe("Pyramid slug identifier") },
  async ({ slug }) => {
    const resp = await pyramidFetch(`/pyramid/${encodeURIComponent(slug)}/apex`);
    return toToolResult(resp);
  }
);

// 4. pyramid_search
server.tool(
  "pyramid_search",
  "Search across a pyramid's knowledge base with a natural language query. Returns matching nodes ranked by relevance.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    query: z.string().describe("Search query (natural language or keywords)"),
  },
  async ({ slug, query }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/search?q=${encodeURIComponent(query)}`
    );
    return toToolResult(resp);
  }
);

// 5. pyramid_drill
server.tool(
  "pyramid_drill",
  "Drill into a specific pyramid node and retrieve it along with its immediate children. Use node IDs from search or apex results.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    node_id: z.string().describe("Node ID to drill into (e.g. L2-000, L1-003, L0-012)"),
  },
  async ({ slug, node_id }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/drill/${encodeURIComponent(node_id)}`
    );
    return toToolResult(resp);
  }
);

// 6. pyramid_faq_match
server.tool(
  "pyramid_faq_match",
  "Find the best matching FAQ entry for a question. Returns the closest match with answer, related nodes, and confidence.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    query: z.string().describe("Question to match against FAQ entries"),
  },
  async ({ slug, query }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/faq/match?q=${encodeURIComponent(query)}`
    );
    return toToolResult(resp);
  }
);

// 7. pyramid_faq_directory
server.tool(
  "pyramid_faq_directory",
  "Get the FAQ directory for a pyramid — shows all FAQ entries organized by category (if available). Returns flat list for small FAQ sets, hierarchical categories for larger ones.",
  { slug: z.string().describe("Pyramid slug identifier") },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/faq/directory`
    );

    // Graceful handling for 404 — endpoint may not exist yet (Phase 4 dependency)
    if (!resp.ok && resp.status === 404) {
      return {
        content: [
          {
            type: "text" as const,
            text: JSON.stringify(
              {
                status: "not_available",
                message:
                  "FAQ directory is not yet available for this pyramid. " +
                  "This feature requires Wire Node v0.2 with the FAQ Knowledge Abstraction module. " +
                  "You can still use pyramid_faq_match to query individual FAQ entries.",
              },
              null,
              2
            ),
          },
        ],
      };
    }

    return toToolResult(resp);
  }
);

// 8. pyramid_annotate
server.tool(
  "pyramid_annotate",
  "Add an annotation to a pyramid node. Annotations capture agent-discovered knowledge, corrections, or insights that enrich the pyramid over time.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    node_id: z.string().describe("Node ID to annotate (e.g. L2-000, L1-003)"),
    content: z.string().describe("Annotation content — the knowledge, correction, or insight to attach"),
    question_context: z
      .string()
      .optional()
      .describe("Optional: the question or task context that prompted this annotation"),
    annotation_type: z
      .string()
      .optional()
      .describe(
        "Optional: annotation type — 'observation', 'correction', 'question', 'friction', 'idea' (default: 'observation')"
      ),
    author: z
      .string()
      .optional()
      .describe("Optional: author identifier (e.g. 'my-agent', 'auditor-1'). Defaults to 'mcp-agent'."),
  },
  async ({ slug, node_id, content, question_context, annotation_type, author }) => {
    const body: Record<string, string> = {
      node_id,
      content,
      author: author || "mcp-agent",
    };
    if (question_context) body.question_context = question_context;
    if (annotation_type) body.annotation_type = annotation_type;

    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/annotate`,
      { method: "POST", body }
    );
    return toToolResult(resp);
  }
);

// ── Main ─────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  // Non-blocking connectivity check (logs warning, doesn't prevent startup)
  await checkConnectivity();

  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error(`[pyramid-mcp] MCP server running (stdio transport)`);
}

main().catch((err) => {
  console.error("[pyramid-mcp] Fatal error:", err);
  process.exit(1);
});
