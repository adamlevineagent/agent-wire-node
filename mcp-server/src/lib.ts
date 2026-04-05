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

// ── Self-Documenting Tool Catalog ───────────────────────────────────────────

export interface CatalogArg {
  name: string;
  type: string;
  required: boolean;
  description: string;
  default?: string;
}

export interface CatalogFlag {
  name: string;
  type: string;
  description: string;
  default?: string;
}

export interface CatalogEntry {
  cli: string;
  mcp: string;
  category: string;
  description: string;
  args: CatalogArg[];
  flags?: CatalogFlag[];
  examples?: string[];
  related?: string[];
}

export const TOOL_CATALOG_VERSION = "0.3.0";

export const TOOL_CATALOG_CATEGORIES: Record<string, string> = {
  core: "System health and pyramid listing",
  exploration: "Navigate and read pyramid content",
  analysis: "Extracted intelligence — entities, terms, corrections, edges, threads",
  operations: "Build status, costs, staleness, auto-update",
  composite: "Multi-endpoint commands that aggregate data",
  question: "Question pyramid creation and querying",
  vine: "Vine conversation system commands",
  annotation: "Read and write annotations on pyramid nodes",
  coordination: "Multi-agent session tracking and coordination",
};

export const TOOL_CATALOG: CatalogEntry[] = [
  // ── Core ──
  {
    cli: "health", mcp: "pyramid_health", category: "core",
    description: "Check if Wire Node is running. Returns server version and status.",
    args: [], examples: ["pyramid-cli health"],
  },
  {
    cli: "slugs", mcp: "pyramid_list_slugs", category: "core",
    description: "List all available pyramid slugs with content types and metadata.",
    args: [], examples: ["pyramid-cli slugs"],
    related: ["apex", "tree"],
  },

  // ── Exploration ──
  {
    cli: "apex", mcp: "pyramid_apex", category: "exploration",
    description: "Get the apex (top-level) node — the system overview with headline, terms, children, and structural summary. Use --summary for a token-efficient version.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "summary", type: "boolean", description: "Strip to headline, distilled, self_prompt, children IDs, and terms only" }],
    examples: ["pyramid-cli apex my-pyramid", "pyramid-cli apex my-pyramid --summary"],
    related: ["tree", "drill", "search", "handoff"],
  },
  {
    cli: "search", mcp: "pyramid_search", category: "exploration",
    description: "Full-text keyword search across pyramid nodes. Results are ranked by depth (L3>L2>L1>L0) then by query term frequency. Returns _hint on 0 results suggesting FAQ. Use --semantic for LLM-backed keyword rewriting when FTS returns 0 results.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "query", type: "string", required: true, description: "Search keywords" },
    ],
    flags: [{ name: "semantic", type: "boolean", description: "Enable LLM keyword rewriting fallback when FTS returns 0 results" }],
    examples: ["pyramid-cli search my-pyramid \"stale engine\"", "pyramid-cli search my-pyramid \"stale engine\" --semantic"],
    related: ["faq", "terms", "drill"],
  },
  {
    cli: "drill", mcp: "pyramid_drill", category: "exploration",
    description: "Drill into a specific node: returns the node, its children, evidence, gaps, and question_context. Enriched with inline annotations and a breadcrumb trail from apex to current node.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "node_id", type: "string", required: true, description: "Node ID (e.g. L2-000, L1-003, L0-012, or cross-slug: source-slug:L1-003)" },
    ],
    examples: ["pyramid-cli drill my-pyramid L1-003"],
    related: ["node", "search", "annotations", "tree"],
  },
  {
    cli: "node", mcp: "pyramid_node", category: "exploration",
    description: "Get a single node by ID without children or evidence. Use drill instead if you need the full context.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "node_id", type: "string", required: true, description: "Node ID" },
    ],
    examples: ["pyramid-cli node my-pyramid L0-012"],
    related: ["drill"],
  },
  {
    cli: "faq", mcp: "pyramid_faq_match", category: "exploration",
    description: "Match a natural-language question against FAQ entries. Without a query, lists all FAQ entries. Returns _hint on 0 matches suggesting search.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "query", type: "string", required: false, description: "Question to match (omit to list all)" },
    ],
    examples: ["pyramid-cli faq my-pyramid \"How does the stale engine work?\"", "pyramid-cli faq my-pyramid"],
    related: ["search", "faq-dir"],
  },
  {
    cli: "faq-dir", mcp: "pyramid_faq_directory", category: "exploration",
    description: "FAQ directory listing. Same as 'faq' without a query. Shows all FAQ entries organized by category.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["faq"],
  },
  {
    cli: "tree", mcp: "pyramid_tree", category: "exploration",
    description: "Full tree structure in one call. Returns all nodes as an indented hierarchy (L3 > L2 > L1 > L0) with headline and child count per node.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli tree my-pyramid"],
    related: ["apex", "drill"],
  },
  {
    cli: "navigate", mcp: "pyramid_navigate", category: "exploration",
    description: "One-shot question answering. Searches for relevant nodes, fetches content, and synthesizes a direct answer with provenance citations. Costs 1 LLM call.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "question", type: "string", required: true, description: "The question to answer" },
    ],
    examples: ["pyramid-cli navigate my-pyramid \"How does the stale engine work?\""],
    related: ["search", "drill", "faq"],
  },

  // ── Analysis ──
  {
    cli: "entities", mcp: "pyramid_entities", category: "analysis",
    description: "All extracted entities (people, systems, concepts) across the pyramid. Find where any entity is mentioned without searching.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["terms", "search"],
  },
  {
    cli: "terms", mcp: "pyramid_terms", category: "analysis",
    description: "Terms dictionary — defined vocabulary with definitions. Essential for cold-start onboarding to learn the pyramid's language.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["entities", "apex", "search"],
  },
  {
    cli: "corrections", mcp: "pyramid_corrections", category: "analysis",
    description: "Correction log — what was wrong in the source material that the pyramid corrected during build. Quality signal.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["resolved", "meta"],
  },
  {
    cli: "edges", mcp: "pyramid_edges", category: "analysis",
    description: "Web edges — all lateral connections between nodes. Cross-cutting themes and relationships without iterative drilling.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["threads", "tree", "drill"],
  },
  {
    cli: "threads", mcp: "pyramid_threads", category: "analysis",
    description: "Thread clusters showing how L0 nodes were grouped into L1 themes. Reveals the pyramid's organizational logic.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["edges", "tree"],
  },
  {
    cli: "meta", mcp: "pyramid_meta", category: "analysis",
    description: "Meta-analysis nodes from post-build passes (webbing, entity resolution). Higher-order structural intelligence.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["corrections", "resolved"],
  },
  {
    cli: "resolved", mcp: "pyramid_resolved", category: "analysis",
    description: "Resolution status across the pyramid. Which questions/gaps have been answered and which remain open.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["corrections", "meta"],
  },

  // ── Operations ──
  {
    cli: "dadbear", mcp: "pyramid_dadbear_status", category: "operations",
    description: "DADBEAR auto-update status: enabled/disabled, last check, debounce, breaker/freeze state.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["cost", "stale-log", "diff"],
  },
  {
    cli: "cost", mcp: "pyramid_cost", category: "operations",
    description: "Token and dollar cost of building this pyramid. Filter by build ID for historical cost.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "build", type: "string", description: "Specific build ID (default: latest)" }],
    examples: ["pyramid-cli cost my-pyramid", "pyramid-cli cost my-pyramid --build abc123"],
    related: ["dadbear", "diff"],
  },
  {
    cli: "stale-log", mcp: "pyramid_stale_log", category: "operations",
    description: "Staleness evaluation history: which nodes were re-evaluated, when, and why. Assess freshness and trust.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "limit", type: "number", description: "Max entries to return", default: "all" }],
    related: ["dadbear", "diff", "usage"],
  },
  {
    cli: "usage", mcp: "pyramid_usage", category: "operations",
    description: "Access pattern statistics: most frequently accessed nodes. Navigation prioritization signal.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "limit", type: "number", description: "Max entries to return", default: "100" }],
    related: ["stale-log"],
  },
  {
    cli: "diff", mcp: "pyramid_diff", category: "operations",
    description: "Changelog approximation: stale-log + build status. See what changed since your last visit.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["stale-log", "dadbear", "cost"],
  },

  // ── Composite ──
  {
    cli: "handoff", mcp: "pyramid_handoff", category: "composite",
    description: "Generate a complete onboarding handoff block. Fetches apex, FAQ, annotations, and DADBEAR status in parallel. Returns CLI command templates, annotation summary, top FAQ questions, and tips.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli handoff my-pyramid"],
    related: ["apex", "faq", "annotations", "dadbear"],
  },
  {
    cli: "compare", mcp: "pyramid_compare", category: "composite",
    description: "Cross-pyramid comparison: shared/unique terms, conflicting definitions, structural differences, decision counts.",
    args: [
      { name: "slug1", type: "string", required: true, description: "First pyramid slug" },
      { name: "slug2", type: "string", required: true, description: "Second pyramid slug" },
    ],
    examples: ["pyramid-cli compare pyramid-a pyramid-b"],
    related: ["apex", "terms"],
  },

  // ── Annotation ──
  {
    cli: "annotations", mcp: "pyramid_annotations", category: "annotation",
    description: "List annotations. Optionally filter to a specific node. Annotations are agent-contributed knowledge, corrections, and insights.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "node_id", type: "string", required: false, description: "Filter to annotations on this node" },
    ],
    related: ["annotate", "drill"],
  },
  {
    cli: "annotate", mcp: "pyramid_annotate", category: "annotation",
    description: "Add an annotation to a node. Captures knowledge, corrections, or insights. Annotations with question_context trigger FAQ creation.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "node_id", type: "string", required: true, description: "Node ID to annotate" },
      { name: "content", type: "string", required: true, description: "Annotation text" },
    ],
    flags: [
      { name: "question", type: "string", description: "Question this answers (triggers FAQ)" },
      { name: "author", type: "string", description: "Your agent name", default: "cli-agent" },
      { name: "type", type: "string", description: "observation | correction | question | friction | idea", default: "observation" },
    ],
    examples: ["pyramid-cli annotate my-pyramid L0-012 \"Finding text\" --question \"What does X do?\" --author my-agent --type observation"],
    related: ["annotations", "drill", "faq"],
  },
  {
    cli: "react", mcp: "pyramid_react", category: "annotation",
    description: "Vote on an annotation (up/down). Each agent can vote once per annotation. Subsequent votes replace the previous one.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "annotation_id", type: "number", required: true, description: "Annotation ID to react to" },
      { name: "reaction", type: "string", required: true, description: "up or down" },
    ],
    flags: [{ name: "agent", type: "string", description: "Your agent identifier", default: "anonymous" }],
    examples: ["pyramid-cli react my-pyramid 42 up --agent my-agent"],
    related: ["annotations", "annotate"],
  },

  // ── Question Pyramid ──
  {
    cli: "create-question-slug", mcp: "pyramid_create_question_slug", category: "question",
    description: "Create a question pyramid slug that references one or more source pyramids. Question slugs compose knowledge across references.",
    args: [{ name: "name", type: "string", required: true, description: "Name for the new question slug" }],
    flags: [{ name: "ref", type: "string", description: "Source slug to reference (repeatable, at least one required)" }],
    examples: ["pyramid-cli create-question-slug my-question --ref source-1 --ref source-2"],
    related: ["question-build", "references", "composed"],
  },
  {
    cli: "question-build", mcp: "pyramid_question_build", category: "question",
    description: "Build a question pyramid: decomposes the question into sub-questions and builds answer nodes across referenced source pyramids.",
    args: [
      { name: "slug", type: "string", required: true, description: "Question pyramid slug" },
      { name: "question", type: "string", required: true, description: "The question to investigate" },
    ],
    flags: [
      { name: "granularity", type: "number", description: "Sub-questions per decomposition level", default: "3" },
      { name: "max-depth", type: "number", description: "Maximum decomposition depth", default: "3" },
    ],
    related: ["create-question-slug", "composed"],
  },
  {
    cli: "references", mcp: "pyramid_references", category: "question",
    description: "Show the reference graph: what this slug references and what references it.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["composed", "create-question-slug"],
  },
  {
    cli: "composed", mcp: "pyramid_composed_view", category: "question",
    description: "Composed view across a question slug and all its referenced source pyramids. Shows all nodes and edges.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    related: ["references", "question-build"],
  },

  // ── Vine ──
  {
    cli: "vine-build", mcp: "", category: "vine",
    description: "Build a vine from JSONL conversation directories.",
    args: [
      { name: "slug", type: "string", required: true, description: "Vine slug" },
      { name: "dirs", type: "string[]", required: true, description: "One or more paths to JSONL directories" },
    ],
  },
  {
    cli: "vine-bunches", mcp: "", category: "vine",
    description: "List all bunches (conversation groups) with metadata.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
    related: ["vine-eras", "vine-threads"],
  },
  {
    cli: "vine-eras", mcp: "", category: "vine",
    description: "List ERA (event-response-action) annotations across the vine.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-decisions", mcp: "", category: "vine",
    description: "List decision FAQ entries extracted from conversations.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-entities", mcp: "", category: "vine",
    description: "List entity resolution FAQ entries from conversations.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-threads", mcp: "", category: "vine",
    description: "List vine thread continuity and web edges between conversation segments.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-drill", mcp: "", category: "vine",
    description: "Directory-wired drill for vine navigation structure.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-rebuild-upper", mcp: "", category: "vine",
    description: "Force rebuild of L2+ layers for a vine.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },
  {
    cli: "vine-integrity", mcp: "", category: "vine",
    description: "Run integrity check on a vine, return validation results.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
  },

  // ── Coordination ──
  {
    cli: "session-register", mcp: "pyramid_session_register", category: "coordination",
    description: "Register an agent session on a pyramid. Other agents can see active sessions.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
    ],
    flags: [{ name: "agent", type: "string", description: "Your agent name", default: "cli-agent" }],
    related: ["sessions"],
  },
  {
    cli: "sessions", mcp: "pyramid_sessions", category: "coordination",
    description: "List recent agent sessions. Shows which agents have been exploring, when they were last active, and how many actions they took.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
    ],
    related: ["session-register"],
  },

  // ── Help (self-referential) ──
  {
    cli: "help", mcp: "pyramid_help", category: "core",
    description: "Self-documenting help system. Returns the full tool catalog as structured JSON. Use 'help <command>' for details on a specific command.",
    args: [
      { name: "command", type: "string", required: false, description: "Specific command to get help for (omit for full catalog)" },
    ],
    examples: ["pyramid-cli help", "pyramid-cli help drill", "pyramid-cli help --category exploration"],
    related: ["health", "slugs"],
  },
];

/** Get the full structured catalog. */
export function getToolCatalog(): {
  version: string;
  total_commands: number;
  categories: Record<string, string>;
  commands: CatalogEntry[];
} {
  return {
    version: TOOL_CATALOG_VERSION,
    total_commands: TOOL_CATALOG.length,
    categories: TOOL_CATALOG_CATEGORIES,
    commands: TOOL_CATALOG,
  };
}

/** Get catalog entries filtered by category. */
export function getToolCatalogByCategory(category: string): CatalogEntry[] {
  return TOOL_CATALOG.filter((e) => e.category === category);
}

/** Get catalog entry for a specific command (by CLI name or MCP tool name). */
export function getToolCatalogEntry(name: string): CatalogEntry | undefined {
  return TOOL_CATALOG.find((e) => e.cli === name || e.mcp === name);
}
