#!/usr/bin/env node

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import {
  WIRE_NODE_BASE_URL,
  resolveAuthToken,
  pyramidFetch as pyramidFetchRaw,
  type PyramidResponse,
  getToolCatalog,
  getToolCatalogEntry,
  getToolCatalogByCategory,
  getAnnotationTypesSync,
  refreshAnnotationTypes,
  validateAnnotationType,
  FALLBACK_ANNOTATION_TYPES,
} from "./lib.js";

// ── Constants ────────────────────────────────────────────────────────────────

const SERVER_NAME = "wire-node-pyramid";
const SERVER_VERSION = "0.3.0";

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

// ── Core Tools ───────────────────────────────────────────────────────────────

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

// 3. pyramid_apex (enhanced with summary_only)
server.tool(
  "pyramid_apex",
  "Get the apex (top-level) node of a pyramid — the system overview and architecture summary. Pass summary_only=true for a compact version with just headline, distilled, self_prompt, children IDs, and terms.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    summary_only: z
      .boolean()
      .optional()
      .describe("When true, return only headline, distilled, self_prompt, children (IDs only), and terms — strips everything else"),
  },
  async ({ slug, summary_only }) => {
    const resp = await pyramidFetch(`/pyramid/${encodeURIComponent(slug)}/apex`);
    if (!resp.ok) return toToolResult(resp);

    if (summary_only) {
      const data = resp.data as Record<string, unknown>;
      const children = Array.isArray(data.children)
        ? (data.children as Array<Record<string, unknown>>).map((c) => ({
            id: c.id,
            headline: c.headline,
          }))
        : data.children;
      const summary: Record<string, unknown> = {
        headline: data.headline,
        distilled: data.distilled,
        self_prompt: data.self_prompt,
        children,
        terms: data.terms,
      };
      return {
        content: [{ type: "text" as const, text: JSON.stringify(summary, null, 2) }],
      };
    }

    return toToolResult(resp);
  }
);

// 4. pyramid_search (enhanced with cross-referral hint)
server.tool(
  "pyramid_search",
  "Keyword-based full-text search across a pyramid's knowledge base. Returns matching nodes ranked by relevance. For natural-language question matching, try pyramid_faq_match instead.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    query: z.string().describe("Search query (natural language or keywords)"),
    semantic: z.boolean().optional().describe("When true and no FTS results found, uses LLM to rewrite query into keywords and retries. Costs 1 LLM call on fallback."),
  },
  async ({ slug, query, semantic }) => {
    const e = encodeURIComponent(slug);
    const semanticParam = semantic ? "&semantic=true" : "";
    const resp = await pyramidFetch(
      `/pyramid/${e}/search?q=${encodeURIComponent(query)}${semanticParam}`
    );

    if (resp.ok) {
      const results = resp.data;
      if (Array.isArray(results) && results.length === 0) {
        return {
          content: [
            {
              type: "text" as const,
              text: JSON.stringify(
                {
                  results: [],
                  _hint:
                    "No keyword matches found. Try pyramid_faq_match for natural-language question matching, or check pyramid_terms for the vocabulary this pyramid uses.",
                },
                null,
                2
              ),
            },
          ],
        };
      }
    }

    return toToolResult(resp);
  }
);

// 5. pyramid_drill (enriched with annotations + breadcrumb)
server.tool(
  "pyramid_drill",
  "Drill into a specific pyramid node and retrieve it along with its immediate children, annotations, and breadcrumb path. Use node IDs from search or apex results. For question slugs, node IDs may use cross-slug handle-path format (e.g. 'source-slug:L1-003').",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    node_id: z
      .string()
      .describe(
        "Node ID to drill into (e.g. L2-000, L1-003, L0-012, or cross-slug: source-slug:L1-003)"
      ),
  },
  async ({ slug, node_id }) => {
    const e = encodeURIComponent(slug);
    const nid = encodeURIComponent(node_id);
    const [drillResp, annotResp] = await Promise.all([
      pyramidFetch(`/pyramid/${e}/drill/${nid}`),
      pyramidFetch(`/pyramid/${e}/annotations?node_id=${nid}`),
    ]);

    if (!drillResp.ok) return toToolResult(drillResp);

    const drillData = drillResp.data as Record<string, unknown>;
    const annotations = annotResp.ok ? annotResp.data : [];
    const annotArr = Array.isArray(annotations) ? annotations : [];

    // Build breadcrumb by walking parent_id
    const breadcrumb: Array<{ id: string; headline: string; depth: number }> = [];
    const node = drillData.node as Record<string, unknown> | undefined;
    if (node) {
      let parentId = node.parent_id as string | null;
      let walks = 0;
      while (parentId && walks < 5) {
        const parentResp = await pyramidFetch(
          `/pyramid/${e}/node/${encodeURIComponent(parentId)}`
        );
        if (!parentResp.ok) break;
        const p = parentResp.data as Record<string, unknown>;
        breadcrumb.unshift({
          id: String(p.id || parentId),
          headline: String(p.headline || ""),
          depth: Number(p.depth ?? 0),
        });
        parentId = (p.parent_id as string) || null;
        walks++;
      }
    }

    // Add current node to breadcrumb trail
    if (node && breadcrumb.length > 0) {
      breadcrumb.push({
        id: String(node.id || node_id),
        headline: String(node.headline || ""),
        depth: Number(node.depth ?? 0),
      });
    }

    // Inject enrichments
    drillData.annotations = annotArr;
    drillData.annotation_count = annotArr.length;
    if (breadcrumb.length > 0) drillData.breadcrumb = breadcrumb;

    return {
      content: [{ type: "text" as const, text: JSON.stringify(drillData, null, 2) }],
    };
  }
);

// 6. pyramid_faq_match (enhanced with cross-referral hint)
server.tool(
  "pyramid_faq_match",
  "Find the best matching FAQ entry for a question. Returns the closest match with answer, related nodes, and confidence. If no matches, try pyramid_search for keyword-based lookup.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    query: z.string().describe("Question to match against FAQ entries"),
  },
  async ({ slug, query }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/faq/match?q=${encodeURIComponent(query)}`
    );

    if (resp.ok) {
      const data = resp.data as Record<string, unknown>;
      const matches = Array.isArray(data) ? data : (data.matches as unknown[]) ?? [];
      if (Array.isArray(matches) && matches.length === 0) {
        return {
          content: [
            {
              type: "text" as const,
              text: JSON.stringify(
                {
                  matches: [],
                  _hint:
                    "No FAQ matches found. Try pyramid_search for keyword-based full-text search, or check pyramid_terms to learn this pyramid's vocabulary.",
                },
                null,
                2
              ),
            },
          ],
        };
      }
    }

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
        // Phase 6c-C: dynamic vocabulary. The cached type list is
        // surfaced in the description so tools see current types at
        // listing time, but validation happens in the handler body
        // (Option C) so operator-published types that arrive between
        // MCP server starts are still accepted via cache-miss refresh.
        `Optional: annotation type (default: 'observation'). Currently known types: ` +
          `${((getAnnotationTypesSync() ?? [...FALLBACK_ANNOTATION_TYPES]).map((t) => `'${t}'`).join(", "))}. ` +
          `New types can be published by writing a vocabulary_entry contribution — ` +
          `they are accepted without a code deploy. Unknown values are rejected ` +
          `by the MCP handler with a helpful error.`
      ),
    author: z
      .string()
      .optional()
      .describe("Optional: author identifier (e.g. 'my-agent', 'auditor-1'). Defaults to 'mcp-agent'."),
  },
  async ({ slug, node_id, content, question_context, annotation_type, author }) => {
    // Phase 6c-C: validate against the dynamic vocabulary cache (with
    // opportunistic refresh on miss) instead of a static z.enum.
    let validatedType: string | undefined;
    if (annotation_type !== undefined) {
      const result = await validateAnnotationType(annotation_type);
      if (!result.ok) {
        return {
          content: [
            {
              type: "text" as const,
              text: JSON.stringify({ error: result.error, validTypes: result.validTypes }, null, 2),
            },
          ],
        };
      }
      validatedType = result.name;
    }

    const body: Record<string, string> = {
      node_id,
      content,
      author: author || "mcp-agent",
    };
    if (question_context) body.question_context = question_context;
    if (validatedType) body.annotation_type = validatedType;

    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/annotate`,
      { method: "POST", body }
    );
    return toToolResult(resp);
  }
);

// 9. pyramid_create_question_slug
server.tool(
  "pyramid_create_question_slug",
  "Create a new question pyramid slug that references one or more existing source pyramids. The question slug composes knowledge across its referenced slugs.",
  {
    slug: z.string().describe("Name for the new question slug"),
    referenced_slugs: z
      .array(z.string())
      .min(1)
      .describe("Array of existing pyramid slugs to reference (at least one required)"),
  },
  async ({ slug, referenced_slugs }) => {
    const resp = await pyramidFetch("/pyramid/slugs", {
      method: "POST",
      body: {
        slug,
        content_type: "question",
        referenced_slugs,
      },
    });
    return toToolResult(resp);
  }
);

// 10. pyramid_question_build
server.tool(
  "pyramid_question_build",
  "Trigger a question build on a question slug. Decomposes the question into sub-questions and builds answer nodes across referenced source pyramids.",
  {
    slug: z.string().describe("Question pyramid slug identifier"),
    question: z.string().describe("The question to decompose and answer"),
    granularity: z
      .number()
      .int()
      .min(1)
      .max(10)
      .optional()
      .describe("Number of sub-questions per decomposition level (default: 3)"),
    max_depth: z
      .number()
      .int()
      .min(1)
      .max(10)
      .optional()
      .describe("Maximum decomposition depth (default: 3)"),
  },
  async ({ slug, question, granularity, max_depth }) => {
    const body: Record<string, unknown> = { question };
    if (granularity !== undefined) body.granularity = granularity;
    if (max_depth !== undefined) body.max_depth = max_depth;

    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/build/question`,
      { method: "POST", body }
    );
    return toToolResult(resp);
  }
);

// 11. pyramid_composed_view
server.tool(
  "pyramid_composed_view",
  "Get a composed view of a question slug, showing all nodes and edges across the slug and its referenced source pyramids.",
  {
    slug: z.string().describe("Pyramid slug identifier (typically a question slug)"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/composed`
    );
    return toToolResult(resp);
  }
);

// 12. pyramid_references
server.tool(
  "pyramid_references",
  "Get the reference graph for a slug — shows what slugs it references and what slugs reference it.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/references`
    );
    return toToolResult(resp);
  }
);

// ── New Single-Endpoint Tools ────────────────────────────────────────────────

// 13. pyramid_node
server.tool(
  "pyramid_node",
  "Get a single pyramid node by ID. Returns the full node content without children. Use pyramid_drill instead if you also need children and evidence.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    node_id: z.string().describe("Node ID to fetch (e.g. L2-000, L1-003, L0-012)"),
  },
  async ({ slug, node_id }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/node/${encodeURIComponent(node_id)}`
    );
    return toToolResult(resp);
  }
);

// 14. pyramid_tree
server.tool(
  "pyramid_tree",
  "Get the full tree structure of a pyramid. Returns all nodes in a hierarchical format with headline and child count per node. Use for a one-call structural overview.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/tree`
    );
    return toToolResult(resp);
  }
);

// 15. pyramid_dadbear_status
server.tool(
  "pyramid_dadbear_status",
  "Get DADBEAR auto-update status for a pyramid. Shows whether auto-update is enabled, last check time, debounce settings, breaker/freeze state.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/auto-update/status`
    );
    return toToolResult(resp);
  }
);

// 16. pyramid_entities
server.tool(
  "pyramid_entities",
  "Get all extracted entities (people, systems, concepts) across the pyramid. Useful for finding where a specific entity is mentioned without searching.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/entities`
    );
    return toToolResult(resp);
  }
);

// 17. pyramid_terms
server.tool(
  "pyramid_terms",
  "Get the terms dictionary for a pyramid. Returns defined vocabulary with definitions. Use this for cold-start onboarding to learn the pyramid's language.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/terms`
    );
    return toToolResult(resp);
  }
);

// 18. pyramid_corrections
server.tool(
  "pyramid_corrections",
  "Get the correction log for a pyramid. Shows what was wrong in the source material that the pyramid corrected during build.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/corrections`
    );
    return toToolResult(resp);
  }
);

// 19. pyramid_edges
server.tool(
  "pyramid_edges",
  "Get all lateral web edges across the pyramid. Shows cross-cutting connections between nodes at the same depth. Use to find thematic relationships.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/edges`
    );
    return toToolResult(resp);
  }
);

// 20. pyramid_threads
server.tool(
  "pyramid_threads",
  "Get thread clusters showing how L0 nodes were grouped into L1 themes. Useful for understanding the pyramid's organizational logic.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/threads`
    );
    return toToolResult(resp);
  }
);

// 21. pyramid_cost
server.tool(
  "pyramid_cost",
  "Get the token and dollar cost report for building this pyramid. Useful for operational planning.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    build_id: z
      .string()
      .optional()
      .describe("Optional: filter to a specific build ID"),
  },
  async ({ slug, build_id }) => {
    let url = `/pyramid/${encodeURIComponent(slug)}/cost`;
    if (build_id) url += `?build_id=${encodeURIComponent(build_id)}`;
    const resp = await pyramidFetch(url);
    return toToolResult(resp);
  }
);

// 22. pyramid_stale_log
server.tool(
  "pyramid_stale_log",
  "Get staleness evaluation history. Shows which nodes were re-evaluated, when, and why. Use to assess pyramid freshness and trust.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    limit: z
      .number()
      .int()
      .optional()
      .describe("Max entries to return (default: 50)"),
  },
  async ({ slug, limit }) => {
    const n = limit ?? 50;
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/stale-log?limit=${n}`
    );
    return toToolResult(resp);
  }
);

// 23. pyramid_usage
server.tool(
  "pyramid_usage",
  "Get access pattern statistics. Shows which nodes are most frequently accessed. Useful for navigation prioritization.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    limit: z
      .number()
      .int()
      .optional()
      .describe("Max entries to return (default: 100)"),
  },
  async ({ slug, limit }) => {
    const n = limit ?? 100;
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/usage?limit=${n}`
    );
    return toToolResult(resp);
  }
);

// 24. pyramid_meta
server.tool(
  "pyramid_meta",
  "Get meta-analysis nodes (post-build passes like webbing and entity resolution). Higher-order structural intelligence about the pyramid.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/meta`
    );
    return toToolResult(resp);
  }
);

// 25. pyramid_resolved
server.tool(
  "pyramid_resolved",
  "Get resolution status across the pyramid. Shows which questions/gaps have been resolved and which remain open.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/resolved`
    );
    return toToolResult(resp);
  }
);

// ── Composite Tools ──────────────────────────────────────────────────────────

// 26. pyramid_handoff
server.tool(
  "pyramid_handoff",
  "Generate a complete onboarding handoff block for a pyramid. Fetches apex, FAQ, annotations, and DADBEAR status in parallel and composes them into a ready-to-paste context block for new agents.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const e = encodeURIComponent(slug);
    const [apexResp, faqResp, annotResp, dadbearResp] = await Promise.all([
      pyramidFetch(`/pyramid/${e}/apex`),
      pyramidFetch(`/pyramid/${e}/faq/directory`),
      pyramidFetch(`/pyramid/${e}/annotations`),
      pyramidFetch(`/pyramid/${e}/auto-update/status`),
    ]);

    const apex = apexResp.ok ? (apexResp.data as Record<string, unknown>) : null;
    const faq = faqResp.ok ? faqResp.data : null;
    const annotations = annotResp.ok ? annotResp.data : [];
    const dadbear = dadbearResp.ok ? dadbearResp.data : null;

    const annotArr = Array.isArray(annotations) ? annotations : [];
    const faqArr = Array.isArray(faq) ? faq : [];

    // Build handoff object
    const handoff: Record<string, unknown> = {
      slug,
      generated_at: new Date().toISOString(),
    };

    if (apex) {
      handoff.headline = apex.headline;
      handoff.distilled = apex.distilled;
      handoff.self_prompt = apex.self_prompt;
      handoff.terms = apex.terms;
      const children = Array.isArray(apex.children)
        ? (apex.children as Array<Record<string, unknown>>).map((c) => ({
            id: c.id,
            headline: c.headline,
          }))
        : [];
      handoff.top_level_sections = children;
    } else {
      handoff.apex_error = "Could not fetch apex node";
    }

    if (faqArr.length > 0) {
      handoff.top_faq_questions = faqArr.slice(0, 10).map((f: unknown) => {
        const entry = f as Record<string, unknown>;
        return { question: entry.question || entry.title, id: entry.id };
      });
    }

    handoff.annotation_summary = {
      total: annotArr.length,
      types: annotArr.reduce(
        (acc: Record<string, number>, a: unknown) => {
          const ann = a as Record<string, unknown>;
          const t = String(ann.annotation_type || "observation");
          acc[t] = (acc[t] || 0) + 1;
          return acc;
        },
        {} as Record<string, number>
      ),
    };

    if (dadbear) {
      handoff.dadbear_status = dadbear;
    }

    handoff.cli_commands = {
      drill: `pyramid_drill(slug="${slug}", node_id="<ID>")`,
      search: `pyramid_search(slug="${slug}", query="<query>")`,
      faq: `pyramid_faq_match(slug="${slug}", query="<question>")`,
      tree: `pyramid_tree(slug="${slug}")`,
      annotate: `pyramid_annotate(slug="${slug}", node_id="<ID>", content="<insight>")`,
    };

    handoff.tips = [
      "Start with pyramid_tree for a structural overview, then drill into sections of interest.",
      "Use pyramid_faq_match for questions about the codebase — it matches against pre-built FAQ entries.",
      "Use pyramid_search for keyword lookups when you know specific terms.",
      "Add annotations with pyramid_annotate to capture discoveries for future agents.",
      "Check pyramid_terms to learn the vocabulary before searching.",
    ];

    return {
      content: [{ type: "text" as const, text: JSON.stringify(handoff, null, 2) }],
    };
  }
);

// 27. pyramid_compare
server.tool(
  "pyramid_compare",
  "Compare two pyramids side by side. Analyzes shared and unique terms, conflicting definitions, and structural differences.",
  {
    slug1: z.string().describe("First pyramid slug"),
    slug2: z.string().describe("Second pyramid slug"),
  },
  async ({ slug1, slug2 }) => {
    const [apex1, apex2] = await Promise.all([
      pyramidFetch(`/pyramid/${encodeURIComponent(slug1)}/apex`),
      pyramidFetch(`/pyramid/${encodeURIComponent(slug2)}/apex`),
    ]);

    if (!apex1.ok || !apex2.ok) {
      const errors: string[] = [];
      if (!apex1.ok) errors.push(`${slug1}: failed to fetch apex (status ${apex1.status})`);
      if (!apex2.ok) errors.push(`${slug2}: failed to fetch apex (status ${apex2.status})`);
      return {
        content: [
          { type: "text" as const, text: JSON.stringify({ error: "Failed to fetch one or both pyramids", details: errors }, null, 2) },
        ],
      };
    }

    const a1 = apex1.data as Record<string, unknown>;
    const a2 = apex2.data as Record<string, unknown>;

    // Compare terms
    const terms1 = (a1.terms || {}) as Record<string, string>;
    const terms2 = (a2.terms || {}) as Record<string, string>;
    const keys1 = new Set(Object.keys(terms1));
    const keys2 = new Set(Object.keys(terms2));
    const shared = Array.from(keys1).filter((k) => keys2.has(k));
    const unique1 = Array.from(keys1).filter((k) => !keys2.has(k));
    const unique2 = Array.from(keys2).filter((k) => !keys1.has(k));
    const conflicts = shared.filter((k) => terms1[k] !== terms2[k]).map((k) => ({
      term: k,
      [slug1]: terms1[k],
      [slug2]: terms2[k],
    }));

    const children1 = Array.isArray(a1.children) ? a1.children : [];
    const children2 = Array.isArray(a2.children) ? a2.children : [];

    const comparison: Record<string, unknown> = {
      slug1: {
        headline: a1.headline,
        child_count: children1.length,
        term_count: keys1.size,
      },
      slug2: {
        headline: a2.headline,
        child_count: children2.length,
        term_count: keys2.size,
      },
      terms_analysis: {
        shared_terms: shared,
        unique_to_slug1: unique1,
        unique_to_slug2: unique2,
        conflicting_definitions: conflicts,
      },
    };

    return {
      content: [{ type: "text" as const, text: JSON.stringify(comparison, null, 2) }],
    };
  }
);

// 28. pyramid_diff
server.tool(
  "pyramid_diff",
  "Get recent changes for a pyramid. Shows staleness log and build status to understand what changed since last visit.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const e = encodeURIComponent(slug);
    const [staleResp, buildResp] = await Promise.all([
      pyramidFetch(`/pyramid/${e}/stale-log?limit=20`),
      pyramidFetch(`/pyramid/${e}/build/status`),
    ]);

    const diff: Record<string, unknown> = {
      slug,
      fetched_at: new Date().toISOString(),
    };

    if (buildResp.ok) {
      diff.build_status = buildResp.data;
    } else {
      diff.build_status = { error: `Could not fetch build status (${buildResp.status})` };
    }

    if (staleResp.ok) {
      diff.recent_stale_evaluations = staleResp.data;
    } else {
      diff.recent_stale_evaluations = { error: `Could not fetch stale log (${staleResp.status})` };
    }

    return {
      content: [{ type: "text" as const, text: JSON.stringify(diff, null, 2) }],
    };
  }
);

// ── Navigate + Agent Coordination Tools ──────────────────────────────────────

// 29. pyramid_navigate
server.tool(
  "pyramid_navigate",
  "One-shot question answering against the pyramid. Searches for relevant nodes, fetches their content, and uses LLM to synthesize a direct answer with provenance citations. Costs 1 LLM call.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    question: z.string().describe("The question to answer"),
  },
  async ({ slug, question }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/navigate`,
      { method: "POST", body: { question } }
    );
    return toToolResult(resp);
  }
);

// 30. pyramid_react
server.tool(
  "pyramid_react",
  "Vote on an annotation (thumbs up or down). Each agent can vote once per annotation; subsequent votes replace the previous one.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    annotation_id: z.number().int().describe("Annotation ID to react to"),
    reaction: z.enum(["up", "down"]).describe("Reaction: 'up' (helpful) or 'down' (unhelpful)"),
    agent_id: z.string().optional().describe("Your agent identifier (default: 'anonymous')"),
  },
  async ({ slug, annotation_id, reaction, agent_id }) => {
    const body: Record<string, unknown> = { reaction };
    if (agent_id) body.agent_id = agent_id;
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/annotations/${annotation_id}/react`,
      { method: "POST", body }
    );
    return toToolResult(resp);
  }
);

// 31. pyramid_session_register
server.tool(
  "pyramid_session_register",
  "Register an agent session on a pyramid. Creates a session entry that other agents can see. Activity is tracked automatically on subsequent requests with the same agent ID.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
    agent_id: z.string().describe("Your unique agent identifier"),
  },
  async ({ slug, agent_id }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/sessions/register`,
      { method: "POST", body: { agent_id } }
    );
    return toToolResult(resp);
  }
);

// 32. pyramid_sessions
server.tool(
  "pyramid_sessions",
  "List recent agent sessions on a pyramid. Shows which agents have been exploring, when they were last active, and how many actions they took.",
  {
    slug: z.string().describe("Pyramid slug identifier"),
  },
  async ({ slug }) => {
    const resp = await pyramidFetch(
      `/pyramid/${encodeURIComponent(slug)}/sessions`
    );
    return toToolResult(resp);
  }
);

// ── Self-Documenting Help ────────────────────────────────────────────────────

// pyramid_help — full catalog or filtered
server.tool(
  "pyramid_help",
  "Self-documenting help system. Returns the full tool catalog as structured JSON with all available commands, their parameters, descriptions, examples, and related commands. Use to discover what the pyramid API can do. Filter by command name or category.",
  {
    command: z
      .string()
      .optional()
      .describe("Specific command/tool name to get help for (e.g. 'drill', 'pyramid_drill'). Omit for full catalog."),
    category: z
      .string()
      .optional()
      .describe("Filter to a category: core, exploration, analysis, operations, composite, question, vine, annotation"),
  },
  async ({ command, category }) => {
    if (command) {
      const entry = getToolCatalogEntry(command);
      if (entry) {
        return {
          content: [{ type: "text" as const, text: JSON.stringify(entry, null, 2) }],
        };
      }
      return {
        content: [{
          type: "text" as const,
          text: JSON.stringify({
            error: `Unknown command: '${command}'`,
            _hint: "Use pyramid_help without arguments to see the full catalog.",
          }, null, 2),
        }],
      };
    }

    if (category) {
      const entries = getToolCatalogByCategory(category);
      if (entries.length > 0) {
        return {
          content: [{ type: "text" as const, text: JSON.stringify({ category, commands: entries }, null, 2) }],
        };
      }
      const catalog = getToolCatalog();
      return {
        content: [{
          type: "text" as const,
          text: JSON.stringify({
            error: `Unknown category: '${category}'`,
            available_categories: Object.keys(catalog.categories),
          }, null, 2),
        }],
      };
    }

    // Full catalog
    return {
      content: [{ type: "text" as const, text: JSON.stringify(getToolCatalog(), null, 2) }],
    };
  }
);

// ── Main ─────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  // Non-blocking connectivity check (logs warning, doesn't prevent startup)
  await checkConnectivity();

  // Phase 6c-C: warm the vocabulary cache. Fire-and-let-fallback; if
  // the Wire node is down, `refreshAnnotationTypes` installs the genesis
  // fallback and emits a warning. We don't await a hard-fail here —
  // graceful-degraded MCP is the goal.
  try {
    const types = await refreshAnnotationTypes();
    console.error(
      `[pyramid-mcp] Vocabulary warmed: ${types.length} annotation_type entries cached`
    );
  } catch (err) {
    console.error(
      `[pyramid-mcp] WARNING: vocabulary warm-up failed: ${err instanceof Error ? err.message : String(err)} — using fallback`
    );
  }

  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error(`[pyramid-mcp] MCP server running (stdio transport)`);
}

main().catch((err) => {
  console.error("[pyramid-mcp] Fatal error:", err);
  process.exit(1);
});
