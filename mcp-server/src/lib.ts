/**
 * Shared helpers for Wire Node pyramid MCP server and CLI.
 * Provides auth token resolution, HTTP fetching, and constants.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

// ── Constants ────────────────────────────────────────────────────────────────

// Base URL for the Wire node HTTP API. Read lazily from
// `PYRAMID_MCP_BASE_URL` at call time so test harnesses that set the
// env AFTER importing still get the override. The `WIRE_NODE_BASE_URL`
// export stays for back-compat with existing callers that read it at
// module scope (they get whatever was set at their first read).
export function getWireNodeBaseUrl(): string {
  return process.env.PYRAMID_MCP_BASE_URL ?? "http://localhost:8765";
}
export const WIRE_NODE_BASE_URL: string = getWireNodeBaseUrl();
export const REQUEST_TIMEOUT_MS = 10_000;

/**
 * Annotation type vocabulary. Phase 6c-C flipped this from a hardcoded
 * `as const` tuple to a dynamic fetch against `GET /vocabulary/annotation_type`
 * (the zero-auth registry endpoint that 6c-A shipped). An operator that
 * publishes a new vocab entry via a contribution write (e.g. `counter_correction`)
 * is now accepted through every TS surface — MCP Zod validation, CLI help
 * text, TOOL_CATALOG render — without a code deploy.
 *
 * The fallback below is genesis parity for the failure-mode case: if the
 * Wire node is down at MCP startup we keep the genesis known-good types so
 * the MCP server doesn't hard-fail. Any drift here silently diverges
 * from the genesis seed; keep the list exactly in lock-step with
 * `src-tauri/src/pyramid/vocab_genesis.rs::GENESIS_ANNOTATION_TYPES`.
 *
 * `AnnotationType` is now a plain `string` alias — the old union-of-literals
 * made consumers reject valid operator-published types at compile time.
 */
export type AnnotationType = string;

/** Genesis fallback. Used ONLY when the vocab fetch fails — keeps MCP
 * alive in a graceful-degraded state. Named decision, see ambiguous
 * decisions in the Phase 6c-C report. */
export const FALLBACK_ANNOTATION_TYPES: readonly string[] = [
  "observation",
  "correction",
  "question",
  "friction",
  "idea",
  "era",
  "transition",
  "health_check",
  "directory",
  "steel_man",
  "red_team",
  "gap",
  "hypothesis",
  "purpose_declaration",
  "purpose_shift",
  "debate_collapse",
];

/** One `VocabEntry` entry as returned by `GET /vocabulary/:vocab_kind`.
 * Mirrors `VocabListItem` in `src-tauri/src/pyramid/vocab_entries.rs`. */
export interface VocabEntry {
  name: string;
  description: string;
  handler_chain_id: string | null;
  reactive: boolean;
  creates_delta: boolean;
  include_in_cascade_prompt?: boolean;
  event_type_on_emit?: string | null;
}

/** Response shape from `GET /vocabulary/:vocab_kind`. */
export interface VocabListResponse {
  vocab_kind: string;
  entries: VocabEntry[];
}

// ── Vocabulary Cache (Phase 6c-C) ────────────────────────────────────────────
// Module-scope Map<vocab_kind, VocabCacheSlot> populated by
// `fetchVocabulary`. Background refresh on a 60s TTL. Opportunistic
// refresh when validation against a fresh caller-provided type fails.

interface VocabCacheSlot {
  entries: VocabEntry[];
  names: Set<string>;
  fetchedAt: number;
  /** Whether the slot is a fallback (fetch failed). Helps callers decide
   * whether to force-refresh on validation miss. */
  isFallback: boolean;
}

const VOCAB_CACHE: Map<string, VocabCacheSlot> = new Map();
const VOCAB_TTL_MS = 60_000; // 60s — vocab changes are rare but not unheard of

/** Fetch a vocab kind from the Wire node. Zero-auth endpoint. Returns
 * the parsed entries on success, or null on failure (caller applies
 * fallback). */
async function fetchVocabRaw(
  vocabKind: string
): Promise<VocabEntry[] | null> {
  const url = `${getWireNodeBaseUrl()}/vocabulary/${encodeURIComponent(vocabKind)}`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  try {
    const resp = await fetch(url, { signal: controller.signal });
    clearTimeout(timeout);
    if (!resp.ok) return null;
    const body = (await resp.json()) as VocabListResponse;
    if (!Array.isArray(body?.entries)) return null;
    return body.entries;
  } catch {
    clearTimeout(timeout);
    return null;
  }
}

/** Populate/refresh a vocab cache slot. Uses the Wire node endpoint
 * and falls back to the hardcoded genesis set on failure (only for
 * `annotation_type` — other kinds get an empty fallback). Returns the
 * slot. */
export async function refreshVocabulary(
  vocabKind: string
): Promise<VocabCacheSlot> {
  const entries = await fetchVocabRaw(vocabKind);
  if (entries !== null) {
    const slot: VocabCacheSlot = {
      entries,
      names: new Set(entries.map((e) => e.name)),
      fetchedAt: Date.now(),
      isFallback: false,
    };
    VOCAB_CACHE.set(vocabKind, slot);
    return slot;
  }

  // Fetch failed — install a fallback. We don't hard-fail MCP startup.
  const fallbackNames =
    vocabKind === "annotation_type" ? [...FALLBACK_ANNOTATION_TYPES] : [];
  const slot: VocabCacheSlot = {
    entries: fallbackNames.map((name) => ({
      name,
      description: "(fallback — Wire node unreachable at fetch time)",
      handler_chain_id: null,
      reactive: false,
      creates_delta: name === "correction",
    })),
    names: new Set(fallbackNames),
    fetchedAt: Date.now(),
    isFallback: true,
  };
  VOCAB_CACHE.set(vocabKind, slot);
  if (!process.env.PYRAMID_MCP_QUIET) {
    console.error(
      `[pyramid] WARNING: vocabulary fetch for '${vocabKind}' failed; ` +
        `cache is serving a ${fallbackNames.length}-entry fallback. ` +
        `Unknown annotation types will be sent to the Wire node for ` +
        `server-side validation until the next successful fetch.`
    );
  }
  return slot;
}

/** Get the current vocab cache slot for a kind, populating or
 * refreshing it if empty / stale. If `forceRefresh` is true, bypass
 * the TTL and hit the network. */
async function getVocabSlot(
  vocabKind: string,
  forceRefresh = false
): Promise<VocabCacheSlot> {
  const existing = VOCAB_CACHE.get(vocabKind);
  const stale =
    !existing ||
    Date.now() - existing.fetchedAt > VOCAB_TTL_MS ||
    existing.isFallback;
  if (!forceRefresh && existing && !stale) {
    return existing;
  }
  return refreshVocabulary(vocabKind);
}

/** Get the currently-cached annotation type names. Dynamic version of
 * the former static `ANNOTATION_TYPES`. Returns the cache (populating
 * if empty) — callers that want a fresh read should first call
 * `refreshAnnotationTypes()`. */
export async function getAnnotationTypes(): Promise<readonly string[]> {
  const slot = await getVocabSlot("annotation_type");
  return Array.from(slot.names).sort();
}

/** Synchronous read from the cache. Returns null if the cache hasn't
 * been populated yet. Used by help-text rendering paths where we don't
 * want to async-wait. */
export function getAnnotationTypesSync(): readonly string[] | null {
  const slot = VOCAB_CACHE.get("annotation_type");
  if (!slot) return null;
  return Array.from(slot.names).sort();
}

/** Force a refresh of the annotation_type vocabulary. Called on
 * startup and on opportunistic validation-miss (e.g. when a caller
 * submits a type that's not in the current cache but may exist in a
 * freshly-published vocab entry). */
export async function refreshAnnotationTypes(): Promise<readonly string[]> {
  const slot = await refreshVocabulary("annotation_type");
  return Array.from(slot.names).sort();
}

/** Validate a candidate annotation type against the cache. If the
 * type is missing from the current cache, force a refresh once and
 * re-check — that handles the "operator just published a new vocab
 * entry; cache is stale" path without a code deploy. Returns the
 * canonical name on success, or a structured error with a helpful
 * hint on failure. */
export async function validateAnnotationType(
  candidate: string
): Promise<
  | { ok: true; name: string }
  | { ok: false; error: string; validTypes: readonly string[] }
> {
  let slot = await getVocabSlot("annotation_type");
  if (!slot.names.has(candidate)) {
    // Cache-miss refresh — maybe an operator just published it.
    slot = await refreshVocabulary("annotation_type");
  }
  if (slot.names.has(candidate)) {
    return { ok: true, name: candidate };
  }
  if (slot.isFallback) {
    return { ok: true, name: candidate };
  }
  const validTypes = Array.from(slot.names).sort();
  return {
    ok: false,
    validTypes,
    error:
      `Unknown annotation_type '${candidate}'. ` +
      `Valid types: ${validTypes.join(", ")}. ` +
      `To add a new type, publish a vocabulary_entry contribution ` +
      `via pyramid-cli vocab publish or POST /api/v1/pyramid/vocabulary — ` +
      `no code deploy required.`,
  };
}

/** Render a pipe-wrapped vocabulary list for help-text surfaces. Takes
 * an explicit list so both sync (post-fetch) and fallback paths can
 * share the wrap logic. */
export function renderVocabTypeList(types: readonly string[], indent: string): string {
  if (types.length === 0) {
    return "(vocab not loaded yet — run `pyramid-cli annotate --help` after Wire node is up)";
  }
  const arr = [...types];
  const first = arr.slice(0, 5).join(" | ");
  const rest = arr.slice(5);
  if (rest.length === 0) return first;
  const mid = rest.slice(0, 4).join(" | ");
  const last = rest.slice(4).join(" | ");
  if (last.length === 0) return `${first} |\n${indent}${mid}`;
  return `${first} |\n${indent}${mid} |\n${indent}${last}`;
}

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
  const url = `${getWireNodeBaseUrl()}${path}`;

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

export const TOOL_CATALOG_VERSION = "0.4.0";

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
  primer: "Primer and slope — onboarding summaries and structural overviews",
  reading: "Reading modes — memoir, walk, thread, decisions, speaker, search",
  manifest: "Manifest and runtime — cold-start bundles and manifest operations",
  vocabulary: "Vocabulary — terms, recognition, and diffs",
  recovery: "Recovery — pyramid recovery status",
  "demand-gen": "Demand generation — job status tracking",
  preview: "Preview — dry-run content processing",
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
      { name: "type", type: "string", description: "__DYNAMIC_ANNOTATION_TYPES__", default: "observation" },
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

  // ── Primer/Slope ──
  {
    cli: "slope", mcp: "pyramid_slope", category: "primer",
    description: "Display slope nodes from the primer. Shows the structural gradient of the pyramid.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli slope my-pyramid"],
    related: ["primer", "apex"],
  },
  {
    cli: "primer", mcp: "pyramid_primer", category: "primer",
    description: "Display formatted primer for onboarding. Optional token budget to control output size.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "budget", type: "number", description: "Token budget for formatted output" }],
    examples: ["pyramid-cli primer my-pyramid", "pyramid-cli primer my-pyramid --budget 2000"],
    related: ["slope", "apex", "handoff"],
  },

  // ── Reading Modes ──
  {
    cli: "memoir", mcp: "pyramid_memoir", category: "reading",
    description: "Memoir reading mode — narrative summary of the pyramid's episodic content.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli memoir my-pyramid"],
    related: ["walk", "thread", "decisions"],
  },
  {
    cli: "walk", mcp: "pyramid_walk", category: "reading",
    description: "Walk reading mode — step through pyramid content layer by layer with direction and limit controls.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [
      { name: "layer", type: "number", description: "Layer number to walk" },
      { name: "direction", type: "string", description: "newest or oldest", default: "newest" },
      { name: "limit", type: "number", description: "Max entries to return" },
    ],
    examples: ["pyramid-cli walk my-pyramid", "pyramid-cli walk my-pyramid --layer 1 --direction oldest --limit 10"],
    related: ["memoir", "thread", "drill"],
  },
  {
    cli: "thread", mcp: "pyramid_thread", category: "reading",
    description: "Thread reading mode — follow a specific identity's contributions through the pyramid.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "identity", type: "string", required: true, description: "Identity to trace" },
    ],
    examples: ["pyramid-cli thread my-pyramid adam"],
    related: ["memoir", "walk", "speaker"],
  },
  {
    cli: "decisions", mcp: "pyramid_decisions", category: "reading",
    description: "Decisions reading mode — extract decision points from the pyramid, optionally filtered by stance.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    flags: [{ name: "stance", type: "string", description: "Filter by decision stance" }],
    examples: ["pyramid-cli decisions my-pyramid", "pyramid-cli decisions my-pyramid --stance approved"],
    related: ["memoir", "walk"],
  },
  {
    cli: "speaker", mcp: "pyramid_speaker", category: "reading",
    description: "Speaker reading mode — view contributions by a specific role/speaker.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "role", type: "string", required: true, description: "Speaker role to filter by" },
    ],
    examples: ["pyramid-cli speaker my-pyramid engineer"],
    related: ["thread", "memoir"],
  },
  {
    cli: "reading-search", mcp: "pyramid_reading_search", category: "reading",
    description: "Reading search mode — search within reading content by query.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "query", type: "string", required: true, description: "Search query" },
    ],
    examples: ["pyramid-cli reading-search my-pyramid \"architecture decision\""],
    related: ["search", "memoir"],
  },

  // ── Manifest/Runtime ──
  {
    cli: "cold-start", mcp: "pyramid_cold_start", category: "manifest",
    description: "Get the cold-start manifest bundle for a pyramid. Everything an agent needs to bootstrap.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli cold-start my-pyramid"],
    related: ["manifest", "primer", "handoff"],
  },
  {
    cli: "manifest", mcp: "pyramid_manifest", category: "manifest",
    description: "Execute manifest operations against a pyramid. POST a JSON array of operations.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "operations", type: "string", required: true, description: "JSON array of manifest operations" },
    ],
    examples: ["pyramid-cli manifest my-pyramid '[{\"op\":\"read\",\"path\":\"apex\"}]'"],
    related: ["cold-start"],
  },

  // ── Vocabulary ──
  {
    cli: "vocab", mcp: "pyramid_vocab", category: "vocabulary",
    description: "Get the full vocabulary for a pyramid — all recognized terms and definitions.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli vocab my-pyramid"],
    related: ["vocab-recognize", "vocab-diff", "terms"],
  },
  {
    cli: "vocab publish", mcp: "", category: "vocabulary",
    description: "Publish a contribution-backed vocabulary_entry into the runtime registry. Prints the local contribution_id. Use only one spelling from each alias group: kind/vocab-kind/type, name/term, description/definition.",
    args: [],
    flags: [
      { name: "kind", type: "string", description: "Vocabulary kind: annotation_type, node_shape, or role_name. Aliases: --vocab-kind, --type; use only one." },
      { name: "vocab-kind", type: "string", description: "Alias for --kind; use only one of --kind, --vocab-kind, --type." },
      { name: "type", type: "string", description: "Alias for --kind; use only one of --kind, --vocab-kind, --type." },
      { name: "name", type: "string", description: "Canonical registry name, max 128 characters. Alias: --term; use only one." },
      { name: "term", type: "string", description: "Alias for --name; use only one of --name, --term." },
      { name: "description", type: "string", description: "Definition shown by /vocabulary/:kind, max 8192 bytes. Alias: --definition; use only one." },
      { name: "definition", type: "string", description: "Alias for --description; use only one of --description, --definition." },
      { name: "handler-chain-id", type: "string", description: "Starter chain binding for reactive entries or roles" },
      { name: "reactive", type: "boolean", description: "Whether annotation arrival emits annotation_reacted" },
      { name: "creates-delta", type: "boolean", description: "Whether annotation save creates a thread delta" },
      { name: "include-in-cascade-prompt", type: "boolean", description: "Whether annotation content flows into ancestor re-distill prompts" },
      { name: "event-type-on-emit", type: "string", description: "Observation event type override" },
      { name: "parent", type: "string", description: "Existing parent entry to validate before publish" },
      { name: "parent-kind", type: "string", description: "Kind for --parent, defaults to --kind" },
    ],
    examples: [
      "pyramid-cli vocab publish --kind annotation_type --name my_custom --description \"Custom annotation\"",
      "pyramid-cli vocab publish --type annotation_type --term my_custom --definition \"Custom annotation\" --reactive true --handler-chain-id starter-debate-steward",
    ],
    related: ["annotate"],
  },
  {
    cli: "vocab-recognize", mcp: "pyramid_vocab_recognize", category: "vocabulary",
    description: "Check if a term is recognized in the pyramid vocabulary.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "term", type: "string", required: true, description: "Term to look up" },
    ],
    examples: ["pyramid-cli vocab-recognize my-pyramid \"action chain\""],
    related: ["vocab", "terms"],
  },
  {
    cli: "vocab-diff", mcp: "pyramid_vocab_diff", category: "vocabulary",
    description: "Get vocabulary changes since a given timestamp or build ID.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "since", type: "string", required: true, description: "Timestamp or build ID to diff from" },
    ],
    examples: ["pyramid-cli vocab-diff my-pyramid 2026-04-01T00:00:00Z"],
    related: ["vocab", "diff"],
  },

  // ── DADBEAR (new endpoints) ──
  {
    cli: "dadbear-status", mcp: "pyramid_dadbear_status_v2", category: "operations",
    description: "DADBEAR status (v2) — detailed auto-update status with breaker state and timing.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli dadbear-status my-pyramid"],
    related: ["dadbear-trigger", "dadbear"],
  },
  {
    cli: "dadbear-trigger", mcp: "pyramid_dadbear_trigger", category: "operations",
    description: "Manually trigger a DADBEAR auto-update check for a pyramid.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli dadbear-trigger my-pyramid"],
    related: ["dadbear-status", "dadbear"],
  },

  // ── Vine Composition ──
  {
    cli: "vine-bedrocks", mcp: "pyramid_vine_bedrocks", category: "vine",
    description: "List bedrock slugs composed into this vine.",
    args: [{ name: "slug", type: "string", required: true, description: "Vine slug" }],
    examples: ["pyramid-cli vine-bedrocks my-vine"],
    related: ["vine-add", "vine-bunches"],
  },
  {
    cli: "vine-add", mcp: "pyramid_vine_add_bedrock", category: "vine",
    description: "Add a bedrock slug to a vine composition.",
    args: [
      { name: "slug", type: "string", required: true, description: "Vine slug" },
      { name: "bedrock_slug", type: "string", required: true, description: "Bedrock slug to add" },
    ],
    examples: ["pyramid-cli vine-add my-vine source-pyramid"],
    related: ["vine-bedrocks"],
  },

  // ── Preview ──
  {
    cli: "preview", mcp: "pyramid_preview", category: "preview",
    description: "Dry-run content processing: preview how a source file would be processed without committing.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "source_path", type: "string", required: true, description: "Path to source file" },
      { name: "content_type", type: "string", required: true, description: "Content type (e.g. markdown, code)" },
    ],
    flags: [{ name: "chain", type: "string", description: "Chain to use for processing" }],
    examples: ["pyramid-cli preview my-pyramid ./doc.md markdown", "pyramid-cli preview my-pyramid ./doc.md markdown --chain custom-chain"],
    related: ["cold-start"],
  },

  // ── Recovery ──
  {
    cli: "recovery-status", mcp: "pyramid_recovery_status", category: "recovery",
    description: "Get recovery status for a pyramid — whether recovery is needed and current state.",
    args: [{ name: "slug", type: "string", required: true, description: "Pyramid slug identifier" }],
    examples: ["pyramid-cli recovery-status my-pyramid"],
    related: ["dadbear-status"],
  },

  // ── Question (new) ──
  {
    cli: "ask", mcp: "pyramid_ask", category: "question",
    description: "Ask a question against a pyramid. Optionally trigger demand generation for unanswered questions.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "question", type: "string", required: true, description: "Question to ask" },
    ],
    flags: [{ name: "demand-gen", type: "boolean", description: "Trigger demand generation if question cannot be answered" }],
    examples: ["pyramid-cli ask my-pyramid \"How does X work?\"", "pyramid-cli ask my-pyramid \"How does X work?\" --demand-gen"],
    related: ["navigate", "faq", "demand-gen-status"],
  },

  // ── Demand Gen ──
  {
    cli: "demand-gen-status", mcp: "pyramid_demand_gen_status", category: "demand-gen",
    description: "Check the status of a demand generation job.",
    args: [
      { name: "slug", type: "string", required: true, description: "Pyramid slug identifier" },
      { name: "job_id", type: "string", required: true, description: "Demand generation job ID" },
    ],
    examples: ["pyramid-cli demand-gen-status my-pyramid job-abc123"],
    related: ["ask"],
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

/** Replace the `__DYNAMIC_ANNOTATION_TYPES__` marker in a catalog entry
 * with the currently-cached list. Rendered at catalog-lookup time so
 * operators who publish a new type see it in `pyramid-cli help annotate`
 * on the next CLI run (cache is per-process). */
function expandDynamicCatalogEntry(entry: CatalogEntry): CatalogEntry {
  if (!entry.flags || entry.flags.length === 0) return entry;
  const types =
    getAnnotationTypesSync() ?? [...FALLBACK_ANNOTATION_TYPES];
  const rendered = types.join(" | ");
  let touched = false;
  const flags = entry.flags.map((f) => {
    if (f.description === "__DYNAMIC_ANNOTATION_TYPES__") {
      touched = true;
      return { ...f, description: rendered };
    }
    return f;
  });
  return touched ? { ...entry, flags } : entry;
}

function expandDynamicCatalog(entries: CatalogEntry[]): CatalogEntry[] {
  return entries.map(expandDynamicCatalogEntry);
}

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
    commands: expandDynamicCatalog(TOOL_CATALOG),
  };
}

/** Get catalog entries filtered by category. */
export function getToolCatalogByCategory(category: string): CatalogEntry[] {
  return expandDynamicCatalog(TOOL_CATALOG.filter((e) => e.category === category));
}

/** Get catalog entry for a specific command (by CLI name or MCP tool name). */
export function getToolCatalogEntry(name: string): CatalogEntry | undefined {
  const entry = TOOL_CATALOG.find((e) => e.cli === name || e.mcp === name);
  return entry ? expandDynamicCatalogEntry(entry) : undefined;
}
