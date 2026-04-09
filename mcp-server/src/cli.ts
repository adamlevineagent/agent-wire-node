#!/usr/bin/env node

/**
 * pyramid-cli — Knowledge Pyramid CLI for agent access
 *
 * Usage:
 *   node dist/cli.js <command> [args] [--compact] [--flag value]
 *
 * Run with --help for full command list, or <command> --help for per-command help.
 * Default output is pretty-printed JSON. Use --compact for minified JSON.
 */

import { resolveAuthToken, pyramidFetch, type PyramidResponse, getToolCatalog, getToolCatalogEntry, getToolCatalogByCategory } from "./lib.js";

// ── Arg Parsing ──────────────────────────────────────────────────────────────

const rawArgs = process.argv.slice(2);

/** Extract --flag value pairs, returning remaining positional args. */
function parseArgs(args: string[]): { positional: string[]; flags: Record<string, string> } {
  const positional: string[] = [];
  const flags: Record<string, string> = {};

  let i = 0;
  while (i < args.length) {
    const arg = args[i];
    if (arg === "-h") {
      flags["help"] = "true";
      i += 1;
    } else if (arg.startsWith("--") && i + 1 < args.length && !args[i + 1].startsWith("--")) {
      flags[arg.slice(2)] = args[i + 1];
      i += 2;
    } else if (arg.startsWith("--")) {
      // Boolean flag (e.g. --compact, --help)
      flags[arg.slice(2)] = "true";
      i += 1;
    } else {
      positional.push(arg);
      i += 1;
    }
  }

  return { positional, flags };
}

const { positional, flags } = parseArgs(rawArgs);
const command = positional[0];

// Pretty is the default; --compact turns it off
const pretty = flags.compact !== "true";

// ── Valid Annotation Types ───────────────────────────────────────────────────

const VALID_ANNOTATION_TYPES = ["observation", "correction", "question", "friction", "idea"] as const;

// ── Per-command Help ─────────────────────────────────────────────────────────

const COMMAND_HELP: Record<string, string> = {
  health: `health — Check if Wire Node is running

Usage: pyramid-cli health

No arguments. Returns health status of the local Wire Node server.`,

  slugs: `slugs — List available pyramids

Usage: pyramid-cli slugs

Returns all pyramid slugs available on this Wire Node.`,

  apex: `apex — Get top-level summary of a pyramid

Usage: pyramid-cli apex <slug> [--summary]

Arguments:
  <slug>      Pyramid slug

Options:
  --summary   Strip response to headline, distilled, self_prompt, children IDs, terms only`,

  search: `search — Search pyramid nodes by keyword

Usage: pyramid-cli search <slug> <query> [--semantic]

Arguments:
  <slug>      Pyramid slug
  <query>     Search query (keywords)

Options:
  --semantic  Enable LLM keyword rewriting fallback when FTS returns 0 results (costs 1 LLM call)

Results are re-ranked client-side by query term frequency in snippets.
If no results, a hint suggests trying the faq command instead.`,

  drill: `drill — Drill into a node and its children

Usage: pyramid-cli drill <slug> <node_id>

Arguments:
  <slug>      Pyramid slug
  <node_id>   Node ID to drill into

Enriches response with annotations for the node and a breadcrumb trail from apex.`,

  node: `node — Get a single node

Usage: pyramid-cli node <slug> <node_id>

Arguments:
  <slug>      Pyramid slug
  <node_id>   Node ID`,

  faq: `faq — Match FAQ or list all entries

Usage: pyramid-cli faq <slug> [query]

Arguments:
  <slug>      Pyramid slug
  [query]     Optional question to match against FAQ entries

Without a query, lists all FAQ entries (same as faq-dir).
With a query, returns matched FAQ entries ranked by relevance.
If no matches, a hint suggests trying the search command instead.`,

  "faq-dir": `faq-dir — FAQ directory view

Usage: pyramid-cli faq-dir <slug>

Arguments:
  <slug>      Pyramid slug

Shows the FAQ directory. This is the same as 'faq <slug>' without a query.
Use 'faq <slug> <question>' to match a specific question.`,

  annotations: `annotations — List annotations

Usage: pyramid-cli annotations <slug> [node_id]

Arguments:
  <slug>      Pyramid slug
  [node_id]   Optional node ID to filter annotations`,

  annotate: `annotate — Add an annotation to a pyramid node

Usage: pyramid-cli annotate <slug> <node_id> <content> [options]

Arguments:
  <slug>      Pyramid slug
  <node_id>   Node ID to annotate
  <content>   Annotation text

Options:
  --question "..."   Question this answers (triggers FAQ creation)
  --author "..."     Your agent name (default: cli-agent)
  --type <type>      observation | correction | question | friction | idea
                     (default: observation)`,

  tree: `tree — Structural overview of a pyramid

Usage: pyramid-cli tree <slug>

Arguments:
  <slug>      Pyramid slug

Returns the full tree structure of the pyramid.`,

  dadbear: `dadbear — DADBEAR auto-update status

Usage: pyramid-cli dadbear <slug>

Arguments:
  <slug>      Pyramid slug

Returns the current DADBEAR auto-update status for the pyramid.`,

  entities: `entities — Entity index

Usage: pyramid-cli entities <slug>

Arguments:
  <slug>      Pyramid slug

Returns all entities extracted from the pyramid.`,

  terms: `terms — Terms dictionary

Usage: pyramid-cli terms <slug>

Arguments:
  <slug>      Pyramid slug

Returns the terms dictionary for the pyramid.`,

  corrections: `corrections — Correction log

Usage: pyramid-cli corrections <slug>

Arguments:
  <slug>      Pyramid slug

Returns the correction log for the pyramid.`,

  edges: `edges — Web edges graph

Usage: pyramid-cli edges <slug>

Arguments:
  <slug>      Pyramid slug

Returns the web edges (cross-references) between nodes.`,

  threads: `threads — Thread clusters

Usage: pyramid-cli threads <slug>

Arguments:
  <slug>      Pyramid slug

Returns thread clusters in the pyramid.`,

  cost: `cost — Build cost report

Usage: pyramid-cli cost <slug> [--build <build_id>]

Arguments:
  <slug>      Pyramid slug

Options:
  --build <id>   Specific build ID to get cost for (default: latest)`,

  "stale-log": `stale-log — Staleness history

Usage: pyramid-cli stale-log <slug> [--limit N]

Arguments:
  <slug>      Pyramid slug

Options:
  --limit <N>   Number of entries to return (default: all)`,

  usage: `usage — Access patterns

Usage: pyramid-cli usage <slug> [--limit N]

Arguments:
  <slug>      Pyramid slug

Options:
  --limit <N>   Number of entries to return (default: 100)`,

  meta: `meta — Meta analysis nodes

Usage: pyramid-cli meta <slug>

Arguments:
  <slug>      Pyramid slug

Returns meta-analysis nodes from the pyramid.`,

  resolved: `resolved — Resolution status

Usage: pyramid-cli resolved <slug>

Arguments:
  <slug>      Pyramid slug

Returns resolution status of the pyramid's questions/issues.`,

  "create-question-slug": `create-question-slug — Create a question pyramid slug

Usage: pyramid-cli create-question-slug <name> --ref <slug1> [--ref <slug2> ...]

Arguments:
  <name>      Name for the new question slug

Options:
  --ref <slug>   Reference slug (at least one required, can repeat)`,

  "question-build": `question-build — Build a question pyramid

Usage: pyramid-cli question-build <slug> "<question>" [options]

Arguments:
  <slug>        Question pyramid slug
  <question>    The question to investigate

Options:
  --granularity <N>   Granularity level (integer)
  --max-depth <N>     Maximum depth (integer)`,

  references: `references — Show references and referrers

Usage: pyramid-cli references <slug>

Arguments:
  <slug>      Pyramid slug

Shows what the slug references and what references it.`,

  composed: `composed — Composed view across referenced slugs

Usage: pyramid-cli composed <slug>

Arguments:
  <slug>      Pyramid slug

Returns a composed view across the slug and its referenced slugs.`,

  handoff: `handoff — Composite handoff block for a pyramid

Usage: pyramid-cli handoff <slug>

Arguments:
  <slug>      Pyramid slug

Fetches apex, FAQ directory, annotations, and DADBEAR status in parallel.
Returns a composite handoff block with:
  - Pyramid headline and slug
  - Pre-filled CLI command templates
  - DADBEAR status summary
  - Annotation summary (total + by type)
  - Top 5 FAQ questions
  - Usage tips`,

  compare: `compare — Cross-pyramid comparison

Usage: pyramid-cli compare <slug1> <slug2>

Arguments:
  <slug1>     First pyramid slug
  <slug2>     Second pyramid slug

Compares two pyramids by headline, terms, children counts, and decisions.`,

  diff: `diff — Changelog approximation

Usage: pyramid-cli diff <slug>

Arguments:
  <slug>      Pyramid slug

Fetches stale-log and build status in parallel to approximate recent changes.`,

  "vine-build": `vine-build — Build vine from JSONL directories

Usage: pyramid-cli vine-build <slug> <dir1> [dir2 ...]

Arguments:
  <slug>      Vine slug
  <dir1>      Path to first JSONL directory
  [dir2...]   Additional JSONL directories`,

  "vine-bunches": `vine-bunches — List all bunches with metadata

Usage: pyramid-cli vine-bunches <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-eras": `vine-eras — List ERA annotations

Usage: pyramid-cli vine-eras <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-decisions": `vine-decisions — List decision FAQ entries

Usage: pyramid-cli vine-decisions <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-entities": `vine-entities — List entity resolution FAQ entries

Usage: pyramid-cli vine-entities <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-threads": `vine-threads — List vine thread continuity + web edges

Usage: pyramid-cli vine-threads <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-drill": `vine-drill — Directory-wired drill

Usage: pyramid-cli vine-drill <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-rebuild-upper": `vine-rebuild-upper — Force rebuild L2+ layers

Usage: pyramid-cli vine-rebuild-upper <slug>

Arguments:
  <slug>      Vine slug`,

  "vine-integrity": `vine-integrity — Run integrity check

Usage: pyramid-cli vine-integrity <slug>

Arguments:
  <slug>      Vine slug`,

  navigate: `navigate — One-shot question answering with provenance

Usage: pyramid-cli navigate <slug> "<question>"

Arguments:
  <slug>        Pyramid slug
  <question>    The question to answer

Searches for relevant nodes, fetches content, and uses LLM to synthesize
a direct answer with provenance citations. Costs 1 LLM call.`,

  react: `react — Vote on an annotation

Usage: pyramid-cli react <slug> <annotation_id> up|down [--agent name]

Arguments:
  <slug>            Pyramid slug
  <annotation_id>   Annotation ID to react to
  up|down           Reaction: 'up' (helpful) or 'down' (unhelpful)

Options:
  --agent <name>    Your agent identifier (default: anonymous)

Each agent can vote once per annotation; subsequent votes replace the previous one.`,

  "session-register": `session-register — Register an agent session

Usage: pyramid-cli session-register <slug> [--agent name]

Arguments:
  <slug>      Pyramid slug

Options:
  --agent <name>    Your agent name (default: cli-agent)

Creates a session entry that other agents can see. Activity is tracked
automatically on subsequent requests with the same agent ID.`,

  sessions: `sessions — List recent agent sessions

Usage: pyramid-cli sessions <slug>

Arguments:
  <slug>      Pyramid slug

Shows which agents have been exploring, when they were last active,
and how many actions they took.`,

  "config-profile": `config-profile — Apply a backend config profile

Usage: pyramid-cli config-profile <name>

Arguments:
  <name>      Profile name (e.g., 'blended' applies profiles/blended.json)

Applies model and context limits from the specified JSON profile.`,

  // ── Episodic Memory Vine commands ──

  slope: `slope — Display slope nodes from the primer

Usage: pyramid-cli slope <slug>

Arguments:
  <slug>      Pyramid slug

Returns slope nodes showing the structural gradient of the pyramid.`,

  primer: `primer — Display formatted primer for onboarding

Usage: pyramid-cli primer <slug> [--budget N]

Arguments:
  <slug>      Pyramid slug

Options:
  --budget <N>   Token budget for formatted output`,

  memoir: `memoir — Memoir reading mode

Usage: pyramid-cli memoir <slug>

Arguments:
  <slug>      Pyramid slug

Returns a narrative summary of the pyramid's episodic content.`,

  walk: `walk — Walk reading mode

Usage: pyramid-cli walk <slug> [--layer N] [--direction newest|oldest] [--limit N]

Arguments:
  <slug>      Pyramid slug

Options:
  --layer <N>         Layer number to walk
  --direction <dir>   newest or oldest (default: newest)
  --limit <N>         Max entries to return`,

  thread: `thread — Thread reading mode

Usage: pyramid-cli thread <slug> <identity>

Arguments:
  <slug>        Pyramid slug
  <identity>    Identity to trace through the pyramid`,

  decisions: `decisions — Decisions reading mode

Usage: pyramid-cli decisions <slug> [--stance X]

Arguments:
  <slug>      Pyramid slug

Options:
  --stance <X>   Filter by decision stance`,

  speaker: `speaker — Speaker reading mode

Usage: pyramid-cli speaker <slug> <role>

Arguments:
  <slug>      Pyramid slug
  <role>      Speaker role to filter by`,

  "reading-search": `reading-search — Reading search mode

Usage: pyramid-cli reading-search <slug> <query>

Arguments:
  <slug>      Pyramid slug
  <query>     Search query within reading content`,

  "cold-start": `cold-start — Get cold-start manifest bundle

Usage: pyramid-cli cold-start <slug>

Arguments:
  <slug>      Pyramid slug

Returns everything an agent needs to bootstrap from this pyramid.`,

  manifest: `manifest — Execute manifest operations

Usage: pyramid-cli manifest <slug> <operations-json>

Arguments:
  <slug>              Pyramid slug
  <operations-json>   JSON array of manifest operations

Example:
  pyramid-cli manifest my-pyramid '[{"op":"read","path":"apex"}]'`,

  vocab: `vocab — Get full vocabulary

Usage: pyramid-cli vocab <slug>

Arguments:
  <slug>      Pyramid slug

Returns all recognized terms and definitions for the pyramid.`,

  "vocab-recognize": `vocab-recognize — Check if a term is recognized

Usage: pyramid-cli vocab-recognize <slug> <term>

Arguments:
  <slug>      Pyramid slug
  <term>      Term to look up`,

  "vocab-diff": `vocab-diff — Vocabulary changes since a point in time

Usage: pyramid-cli vocab-diff <slug> <since>

Arguments:
  <slug>      Pyramid slug
  <since>     Timestamp or build ID to diff from`,

  "dadbear-status": `dadbear-status — DADBEAR status (v2)

Usage: pyramid-cli dadbear-status <slug>

Arguments:
  <slug>      Pyramid slug

Returns detailed auto-update status with breaker state and timing.`,

  "dadbear-trigger": `dadbear-trigger — Trigger DADBEAR auto-update

Usage: pyramid-cli dadbear-trigger <slug>

Arguments:
  <slug>      Pyramid slug

Manually triggers a DADBEAR auto-update check.`,

  "vine-bedrocks": `vine-bedrocks — List bedrock slugs in vine

Usage: pyramid-cli vine-bedrocks <slug>

Arguments:
  <slug>      Vine slug

Lists all bedrock slugs composed into this vine.`,

  "vine-add": `vine-add — Add bedrock to vine

Usage: pyramid-cli vine-add <slug> <bedrock-slug>

Arguments:
  <slug>            Vine slug
  <bedrock-slug>    Bedrock slug to add`,

  preview: `preview — Dry-run content processing

Usage: pyramid-cli preview <slug> <source-path> <content-type> [--chain X]

Arguments:
  <slug>            Pyramid slug
  <source-path>     Path to source file
  <content-type>    Content type (e.g. markdown, code)

Options:
  --chain <X>       Chain to use for processing`,

  "recovery-status": `recovery-status — Recovery status

Usage: pyramid-cli recovery-status <slug>

Arguments:
  <slug>      Pyramid slug

Returns whether recovery is needed and current recovery state.`,

  ask: `ask — Ask a question against a pyramid

Usage: pyramid-cli ask <slug> "<question>" [--demand-gen]

Arguments:
  <slug>        Pyramid slug
  <question>    Question to ask

Options:
  --demand-gen  Trigger demand generation if question cannot be answered`,

  "demand-gen-status": `demand-gen-status — Check demand generation job status

Usage: pyramid-cli demand-gen-status <slug> <job-id>

Arguments:
  <slug>      Pyramid slug
  <job-id>    Demand generation job ID`,
};

// ── Auth ─────────────────────────────────────────────────────────────────────

// Track auth source for --verbose
let authSource = "unknown";
const envToken = process.env.PYRAMID_AUTH_TOKEN;
if (envToken) {
  authSource = "env:PYRAMID_AUTH_TOKEN";
}
// resolveAuthToken() handles the full resolution; we just track the source
const AUTH_TOKEN = resolveAuthToken();
if (authSource === "unknown") {
  authSource = "config:~/Library/Application Support/wire-node/pyramid_config.json";
}

// --verbose: print auth method to stderr
if (flags.verbose === "true") {
  process.stderr.write(`[verbose] Auth resolved via ${authSource}\n`);
}

/** Shorthand that injects the auth token. */
function pf(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<PyramidResponse> {
  return pyramidFetch(path, { ...options, authToken: AUTH_TOKEN });
}

// ── Output ───────────────────────────────────────────────────────────────────

function output(resp: PyramidResponse, slug?: string): void {
  if (!resp.ok) {
    const payload =
      typeof resp.data === "object" && resp.data !== null
        ? resp.data
        : { error: String(resp.data), status: resp.status };

    // Enhanced error messages: add context hints
    const errorStr = JSON.stringify(payload);
    if (errorStr.toLowerCase().includes("not found") && slug) {
      const enhanced = typeof payload === "object" && payload !== null
        ? { ...payload as Record<string, unknown>, _hint: `Pyramid '${slug}' not found. Run 'pyramid-cli slugs' to see available pyramids.` }
        : payload;
      process.stderr.write(JSON.stringify(enhanced, null, 2) + "\n");
    } else {
      process.stderr.write(JSON.stringify(payload, null, 2) + "\n");
    }
    process.exit(1);
  }

  const text =
    typeof resp.data === "string"
      ? resp.data
      : JSON.stringify(resp.data, pretty ? null : undefined, pretty ? 2 : undefined);
  process.stdout.write(text + "\n");
}

/** Output raw data (not a PyramidResponse). Respects --compact flag. */
function outputData(data: unknown): void {
  const text =
    typeof data === "string"
      ? data
      : JSON.stringify(data, pretty ? null : undefined, pretty ? 2 : undefined);
  process.stdout.write(text + "\n");
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function requireArg(index: number, name: string): string {
  const val = positional[index];
  if (!val) {
    process.stderr.write(`Error: missing required argument <${name}>\n`);
    process.exit(1);
  }
  return val;
}

function enc(s: string): string {
  return encodeURIComponent(s);
}

// ── Command Dispatch ─────────────────────────────────────────────────────────

async function run(): Promise<void> {
  // Handle --help, -h, or no command
  if (!command || (flags.help === "true" && !command)) {
    printUsage();
    if (!command) process.exit(1);
    process.exit(0);
  }

  // Per-command help: if command exists and --help is set, show command-specific help
  if (flags.help === "true" && command) {
    const help = COMMAND_HELP[command];
    if (help) {
      process.stderr.write(help + "\n");
      process.exit(0);
    } else {
      process.stderr.write(`No help available for '${command}'. Run 'pyramid-cli --help' for all commands.\n`);
      process.exit(1);
    }
  }

  switch (command) {
    case "config-profile": {
      const name = requireArg(1, "name");
      output(await pf(`/pyramid/config/profile/${enc(name)}`, { method: "POST" }));
      break;
    }

    case "health": {
      output(await pf("/health"));
      break;
    }

    case "slugs": {
      output(await pf("/pyramid/slugs"));
      break;
    }

    case "apex": {
      const slug = requireArg(1, "slug");
      const resp = await pf(`/pyramid/${enc(slug)}/apex`);

      if (resp.ok && flags.summary === "true") {
        // Strip to summary fields only
        const full = resp.data as Record<string, unknown>;
        const children = Array.isArray(full.children)
          ? (full.children as Array<Record<string, unknown>>).map((c) => c.id ?? c)
          : full.children;
        const summary: Record<string, unknown> = {
          headline: full.headline,
          distilled: full.distilled,
          self_prompt: full.self_prompt,
          children,
          terms: full.terms,
        };
        outputData(summary);
      } else {
        output(resp, slug);
      }
      break;
    }

    case "search": {
      const slug = requireArg(1, "slug");
      const query = requireArg(2, "query");
      const semanticFlag = flags.semantic === "true" ? "&semantic=true" : "";
      const resp = await pf(`/pyramid/${enc(slug)}/search?q=${enc(query)}${semanticFlag}`);

      if (resp.ok) {
        const data = resp.data;
        // Check for empty results
        if (Array.isArray(data) && data.length === 0) {
          outputData({
            results: [],
            _hint: `No keyword matches found. Try: pyramid-cli faq ${slug} "${query}" for natural-language FAQ matching.`,
          });
        } else if (Array.isArray(data) && data.length > 0) {
          // Client-side re-ranking: boost by query term frequency in snippet
          const queryTerms = query.toLowerCase().split(/\s+/).filter(Boolean);
          const reranked = data.map((item: Record<string, unknown>) => {
            const snippet = String(item.snippet ?? item.headline ?? "").toLowerCase();
            let matchCount = 0;
            for (const term of queryTerms) {
              if (snippet.includes(term)) matchCount++;
            }
            const originalScore = typeof item.score === "number" ? item.score : 1;
            return { ...item, _reranked_score: originalScore * (1 + matchCount / 10) };
          });
          reranked.sort((a: Record<string, unknown>, b: Record<string, unknown>) =>
            (b._reranked_score as number) - (a._reranked_score as number)
          );
          outputData(reranked);
        } else {
          output(resp, slug);
        }
      } else {
        output(resp, slug);
      }
      break;
    }

    case "drill": {
      const slug = requireArg(1, "slug");
      const nodeId = requireArg(2, "node_id");

      // Fetch drill + annotations in parallel
      const [drillResp, annotResp] = await Promise.all([
        pf(`/pyramid/${enc(slug)}/drill/${enc(nodeId)}`),
        pf(`/pyramid/${enc(slug)}/annotations?node_id=${enc(nodeId)}`),
      ]);

      if (drillResp.ok) {
        const drillData: Record<string, unknown> = typeof drillResp.data === "object" && drillResp.data !== null
          ? { ...drillResp.data as Record<string, unknown> }
          : { result: drillResp.data };

        // Inject annotations
        if (annotResp.ok && Array.isArray(annotResp.data)) {
          drillData.annotations = annotResp.data;
          drillData.annotation_count = annotResp.data.length;
        } else {
          drillData.annotations = [];
          drillData.annotation_count = 0;
        }

        // Build breadcrumb by walking parent_id
        // The drill response wraps the node: { node: {...}, children: [...], ... }
        const nodeObj = (drillData.node ?? drillData) as Record<string, unknown>;
        const depth = typeof nodeObj.depth === "number" ? nodeObj.depth : 0;

        if (depth > 0) {
          const breadcrumb: Array<{ id: string; headline: string; depth: number }> = [];
          let currentParentId = nodeObj.parent_id as string | null | undefined;
          let iterations = 0;
          const MAX_BREADCRUMB_WALK = 5;

          while (currentParentId && iterations < MAX_BREADCRUMB_WALK) {
            const parentResp = await pf(`/pyramid/${enc(slug)}/node/${enc(currentParentId)}`);
            if (!parentResp.ok) break;
            const parentNode = parentResp.data as Record<string, unknown>;
            breadcrumb.unshift({
              id: String(parentNode.id ?? currentParentId),
              headline: String(parentNode.headline ?? ""),
              depth: typeof parentNode.depth === "number" ? parentNode.depth : 0,
            });
            currentParentId = parentNode.parent_id as string | null | undefined;
            iterations++;
          }

          // Add current node at the end
          breadcrumb.push({
            id: String(nodeObj.id ?? nodeId),
            headline: String(nodeObj.headline ?? ""),
            depth,
          });

          drillData.breadcrumb = breadcrumb;
        }

        outputData(drillData);
      } else {
        output(drillResp, slug);
      }
      break;
    }

    case "node": {
      const slug = requireArg(1, "slug");
      const nodeId = requireArg(2, "node_id");
      output(await pf(`/pyramid/${enc(slug)}/node/${enc(nodeId)}`), slug);
      break;
    }

    case "faq": {
      const slug = requireArg(1, "slug");
      const query = positional[2]; // optional
      if (query) {
        const resp = await pf(`/pyramid/${enc(slug)}/faq/match?q=${enc(query)}`);
        // Fix #5: handle null/empty FAQ response
        if (resp.ok && (resp.data === null || resp.data === undefined)) {
          outputData({
            matches: [],
            message: "No FAQ entries matched your query.",
            _hint: `No FAQ matches found. Try: pyramid-cli search ${slug} "${query}" for full-text keyword search.`,
          });
        } else if (resp.ok) {
          // Check if result is empty array
          const data = resp.data;
          if (Array.isArray(data) && data.length === 0) {
            outputData({
              matches: [],
              _hint: `No FAQ matches found. Try: pyramid-cli search ${slug} "${query}" for full-text keyword search.`,
            });
          } else if (typeof data === "object" && data !== null) {
            const obj = data as Record<string, unknown>;
            const matches = obj.matches ?? obj.results ?? data;
            if (Array.isArray(matches) && matches.length === 0) {
              outputData({
                matches: [],
                _hint: `No FAQ matches found. Try: pyramid-cli search ${slug} "${query}" for full-text keyword search.`,
              });
            } else {
              output(resp, slug);
            }
          } else {
            output(resp, slug);
          }
        } else {
          output(resp, slug);
        }
      } else {
        output(await pf(`/pyramid/${enc(slug)}/faq/directory`), slug);
      }
      break;
    }

    case "faq-dir": {
      const slug = requireArg(1, "slug");
      const resp = await pf(`/pyramid/${enc(slug)}/faq/directory`);
      if (resp.ok) {
        const data: Record<string, unknown> = typeof resp.data === "object" && resp.data !== null
          ? { ...resp.data as Record<string, unknown> }
          : { result: resp.data };
        data._note = "This is the same as 'faq <slug>' without a query. Use 'faq <slug> <question>' to match a specific question.";
        outputData(data);
      } else {
        output(resp, slug);
      }
      break;
    }

    case "annotations": {
      const slug = requireArg(1, "slug");
      const nodeId = positional[2]; // optional
      const path = nodeId
        ? `/pyramid/${enc(slug)}/annotations?node_id=${enc(nodeId)}`
        : `/pyramid/${enc(slug)}/annotations`;
      output(await pf(path), slug);
      break;
    }

    case "annotate": {
      const slug = requireArg(1, "slug");
      const nodeId = requireArg(2, "node_id");
      const content = requireArg(3, "content");

      // Fix #4: validate --type values
      const annotationType = flags.type || undefined;
      if (annotationType && !(VALID_ANNOTATION_TYPES as readonly string[]).includes(annotationType)) {
        process.stderr.write(
          `Error: invalid annotation type '${annotationType}'.\n` +
          `Valid types: ${VALID_ANNOTATION_TYPES.join(", ")}\n`
        );
        process.exit(1);
      }

      // Fix #8: default to "observation" with note when --type not specified
      const typeDefaulted = !annotationType;
      const resolvedType = annotationType || "observation";

      const body: Record<string, string> = {
        node_id: nodeId,
        content,
        author: flags.author || "cli-agent",
        annotation_type: resolvedType,
      };
      if (flags.question) body.question_context = flags.question;

      const resp = await pf(`/pyramid/${enc(slug)}/annotate`, { method: "POST", body });

      if (resp.ok) {
        // Fix #7 & #8: append integration note to successful annotation response
        const responseData: Record<string, unknown> = typeof resp.data === "object" && resp.data !== null
          ? { ...resp.data as Record<string, unknown> }
          : { result: resp.data };

        if (typeDefaulted) {
          responseData._note = "No --type specified, defaulted to 'observation'. Annotation saved. It is immediately visible via 'annotations' and 'drill'. If it includes a question_context, FAQ processing runs in the background.";
        } else {
          responseData._note = "Annotation saved. It is immediately visible via 'annotations' and 'drill'. If it includes a question_context, FAQ processing runs in the background.";
        }

        outputData(responseData);
      } else {
        output(resp, slug);
      }
      break;
    }

    // ── Question Pyramid commands (WS8-H) ─────────────────────────────

    case "create-question-slug": {
      const name = requireArg(1, "name");
      // Collect all --ref flags: parseArgs only captures the last --ref,
      // so we manually gather all of them from rawArgs.
      const refs: string[] = [];
      for (let ri = 0; ri < rawArgs.length; ri++) {
        if (rawArgs[ri] === "--ref" && ri + 1 < rawArgs.length) {
          refs.push(rawArgs[ri + 1]);
          ri++;
        }
      }
      if (refs.length === 0) {
        process.stderr.write("Error: at least one --ref <slug> is required\n");
        process.exit(1);
      }
      output(await pf("/pyramid/slugs", {
        method: "POST",
        body: {
          slug: name,
          content_type: "question",
          referenced_slugs: refs,
        },
      }));
      break;
    }

    case "question-build": {
      const slug = requireArg(1, "slug");
      const question = requireArg(2, "question");
      const body: Record<string, unknown> = { question };
      if (flags.granularity) body.granularity = parseInt(flags.granularity, 10);
      if (flags["max-depth"]) body.max_depth = parseInt(flags["max-depth"], 10);
      output(await pf(`/pyramid/${enc(slug)}/build/question`, {
        method: "POST",
        body,
      }));
      break;
    }

    case "references": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/references`), slug);
      break;
    }

    case "composed": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/composed`), slug);
      break;
    }

    // ── Vine Conversation System commands ─────────────────────────────

    case "vine-build": {
      const slug = requireArg(1, "vine_slug");
      const dirs = positional.slice(2);
      if (dirs.length === 0) {
        process.stderr.write("Error: at least one JSONL directory is required\n");
        process.exit(1);
      }
      output(await pf("/pyramid/vine/build", {
        method: "POST",
        body: { vine_slug: slug, jsonl_dirs: dirs },
      }));
      break;
    }

    case "vine-bunches": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/bunches`));
      break;
    }

    case "vine-eras": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/eras`));
      break;
    }

    case "vine-decisions": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/decisions`));
      break;
    }

    case "vine-entities": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/entities`));
      break;
    }

    case "vine-threads": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/threads`));
      break;
    }

    case "vine-drill": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/drill`));
      break;
    }

    case "vine-rebuild-upper": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/rebuild-upper`, { method: "POST" }));
      break;
    }

    case "vine-integrity": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/integrity`, { method: "POST" }));
      break;
    }

    // ── Simple Route Commands (analysis + operations) ─────────────────

    case "tree": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/tree`), slug);
      break;
    }

    case "dadbear": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/auto-update/status`), slug);
      break;
    }

    case "entities": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/entities`), slug);
      break;
    }

    case "terms": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/terms`), slug);
      break;
    }

    case "corrections": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/corrections`), slug);
      break;
    }

    case "edges": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/edges`), slug);
      break;
    }

    case "threads": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/threads`), slug);
      break;
    }

    case "cost": {
      const slug = requireArg(1, "slug");
      const buildId = flags.build;
      const path = buildId
        ? `/pyramid/${enc(slug)}/cost?build_id=${enc(buildId)}`
        : `/pyramid/${enc(slug)}/cost`;
      output(await pf(path), slug);
      break;
    }

    case "stale-log": {
      const slug = requireArg(1, "slug");
      const limit = flags.limit;
      const path = limit
        ? `/pyramid/${enc(slug)}/stale-log?limit=${enc(limit)}`
        : `/pyramid/${enc(slug)}/stale-log`;
      output(await pf(path), slug);
      break;
    }

    case "usage": {
      const slug = requireArg(1, "slug");
      const limit = flags.limit || "100";
      output(await pf(`/pyramid/${enc(slug)}/usage?limit=${enc(limit)}`), slug);
      break;
    }

    case "meta": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/meta`), slug);
      break;
    }

    case "resolved": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/resolved`), slug);
      break;
    }

    // ── Composite Commands ────────────────────────────────────────────

    case "handoff": {
      const slug = requireArg(1, "slug");

      const [apexResp, faqResp, annotResp, dadbearResp] = await Promise.all([
        pf(`/pyramid/${enc(slug)}/apex`),
        pf(`/pyramid/${enc(slug)}/faq/directory`),
        pf(`/pyramid/${enc(slug)}/annotations`),
        pf(`/pyramid/${enc(slug)}/auto-update/status`),
      ]);

      if (!apexResp.ok) {
        output(apexResp, slug);
        break;
      }

      const apex = apexResp.data as Record<string, unknown>;

      // Build annotation summary
      let annotationTotal = 0;
      const byType: Record<string, number> = {};
      if (annotResp.ok && Array.isArray(annotResp.data)) {
        annotationTotal = annotResp.data.length;
        for (const ann of annotResp.data as Array<Record<string, unknown>>) {
          const t = String(ann.annotation_type ?? "unknown");
          byType[t] = (byType[t] || 0) + 1;
        }
      }

      // Build top FAQ questions
      let topFaqQuestions: string[] = [];
      if (faqResp.ok) {
        const faqData = faqResp.data;
        let entries: Array<Record<string, unknown>> = [];
        if (Array.isArray(faqData)) {
          entries = faqData;
        } else if (typeof faqData === "object" && faqData !== null) {
          const fd = faqData as Record<string, unknown>;
          if (Array.isArray(fd.entries)) entries = fd.entries as Array<Record<string, unknown>>;
          else if (Array.isArray(fd.questions)) entries = fd.questions as Array<Record<string, unknown>>;
          else if (Array.isArray(fd.items)) entries = fd.items as Array<Record<string, unknown>>;
        }
        topFaqQuestions = entries
          .slice(0, 5)
          .map((e) => String(e.question ?? e.title ?? e.headline ?? ""))
          .filter(Boolean);
      }

      // DADBEAR summary
      let dadbearSummary: unknown = null;
      if (dadbearResp.ok) {
        dadbearSummary = dadbearResp.data;
      }

      const handoff = {
        slug,
        pyramid_headline: apex.headline ?? null,
        cli_commands: {
          apex: `pyramid-cli apex ${slug}`,
          search: `pyramid-cli search ${slug} "<query>"`,
          drill: `pyramid-cli drill ${slug} <node_id>`,
          faq: `pyramid-cli faq ${slug} "<question>"`,
          tree: `pyramid-cli tree ${slug}`,
          annotations: `pyramid-cli annotations ${slug}`,
          entities: `pyramid-cli entities ${slug}`,
          terms: `pyramid-cli terms ${slug}`,
          cost: `pyramid-cli cost ${slug}`,
        },
        dadbear_status: dadbearSummary,
        annotation_summary: {
          total: annotationTotal,
          by_type: byType,
        },
        top_faq_questions: topFaqQuestions,
        tips: [
          "Use 'drill' to navigate the pyramid tree structure depth-first.",
          "Use 'search' for keyword matching, 'faq' for natural-language question matching.",
          "Annotations are immediately visible after creation via 'annotations' and 'drill'.",
          "Use 'tree' for a structural overview before drilling.",
          "Use 'cost' to check build token spend.",
        ],
      };

      outputData(handoff);
      break;
    }

    case "compare": {
      const slug1 = requireArg(1, "slug1");
      const slug2 = requireArg(2, "slug2");

      const [apex1Resp, apex2Resp] = await Promise.all([
        pf(`/pyramid/${enc(slug1)}/apex`),
        pf(`/pyramid/${enc(slug2)}/apex`),
      ]);

      if (!apex1Resp.ok) {
        output(apex1Resp, slug1);
        break;
      }
      if (!apex2Resp.ok) {
        output(apex2Resp, slug2);
        break;
      }

      const apex1 = apex1Resp.data as Record<string, unknown>;
      const apex2 = apex2Resp.data as Record<string, unknown>;

      // Compare terms
      const terms1 = Array.isArray(apex1.terms) ? apex1.terms.map(String) : [];
      const terms2 = Array.isArray(apex2.terms) ? apex2.terms.map(String) : [];
      const terms1Set = new Set(terms1.map((t: string) => t.toLowerCase()));
      const terms2Set = new Set(terms2.map((t: string) => t.toLowerCase()));
      const shared = terms1.filter((t: string) => terms2Set.has(t.toLowerCase()));
      const uniqueTo1 = terms1.filter((t: string) => !terms2Set.has(t.toLowerCase()));
      const uniqueTo2 = terms2.filter((t: string) => !terms1Set.has(t.toLowerCase()));

      // Compare children counts
      const children1 = Array.isArray(apex1.children) ? apex1.children.length : 0;
      const children2 = Array.isArray(apex2.children) ? apex2.children.length : 0;

      // Compare decisions if present
      const decisions1 = Array.isArray(apex1.decisions) ? apex1.decisions : [];
      const decisions2 = Array.isArray(apex2.decisions) ? apex2.decisions : [];

      const comparison = {
        slug1,
        slug2,
        headlines: {
          [slug1]: apex1.headline ?? null,
          [slug2]: apex2.headline ?? null,
        },
        terms: {
          shared,
          [`unique_to_${slug1}`]: uniqueTo1,
          [`unique_to_${slug2}`]: uniqueTo2,
        },
        children_count: {
          [slug1]: children1,
          [slug2]: children2,
        },
        decisions: {
          [slug1]: decisions1.length,
          [slug2]: decisions2.length,
        },
      };

      outputData(comparison);
      break;
    }

    case "diff": {
      const slug = requireArg(1, "slug");

      const [staleResp, buildResp] = await Promise.all([
        pf(`/pyramid/${enc(slug)}/stale-log`),
        pf(`/pyramid/${enc(slug)}/build/status`),
      ]);

      const result: Record<string, unknown> = { slug };

      if (staleResp.ok) {
        result.recent_changes = staleResp.data;
      } else {
        result.recent_changes = null;
        result._stale_log_error = staleResp.data;
      }

      if (buildResp.ok) {
        result.build_status = buildResp.data;
      } else {
        result.build_status = null;
        result._build_status_error = buildResp.data;
      }

      outputData(result);
      break;
    }

    // ── Self-Documenting Help ─────────────────────────────────────────

    case "help": {
      const topic = positional[1]; // optional: command name
      const categoryFilter = flags.category;

      if (topic) {
        // Help for a specific command
        const entry = getToolCatalogEntry(topic);
        if (entry) {
          outputData(entry);
        } else {
          process.stderr.write(`Unknown command: '${topic}'. Run 'pyramid-cli help' for the full catalog.\n`);
          process.exit(1);
        }
      } else if (categoryFilter) {
        // Filter by category
        const entries = getToolCatalogByCategory(categoryFilter);
        if (entries.length > 0) {
          outputData({ category: categoryFilter, commands: entries });
        } else {
          const catalog = getToolCatalog();
          process.stderr.write(
            `Unknown category: '${categoryFilter}'. Available: ${Object.keys(catalog.categories).join(", ")}\n`
          );
          process.exit(1);
        }
      } else {
        // Full catalog
        outputData(getToolCatalog());
      }
      break;
    }

    case "navigate": {
      const slug = requireArg(1, "slug");
      const question = requireArg(2, "question");
      output(await pf(`/pyramid/${enc(slug)}/navigate`, { method: "POST", body: { question } }), slug);
      break;
    }

    case "react": {
      const slug = requireArg(1, "slug");
      const annotationId = requireArg(2, "annotation_id");
      const reaction = requireArg(3, "reaction");
      if (reaction !== "up" && reaction !== "down") {
        process.stderr.write("Error: reaction must be 'up' or 'down'\n");
        process.exit(1);
      }
      const body: Record<string, string> = { reaction };
      if (flags.agent) body.agent_id = flags.agent;
      output(await pf(`/pyramid/${enc(slug)}/annotations/${enc(annotationId)}/react`, { method: "POST", body }), slug);
      break;
    }

    case "session-register": {
      const slug = requireArg(1, "slug");
      const agentId = flags.agent || "cli-agent";
      output(await pf(`/pyramid/${enc(slug)}/sessions/register`, { method: "POST", body: { agent_id: agentId } }), slug);
      break;
    }

    case "sessions": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/sessions`), slug);
      break;
    }

    // ── Episodic Memory Vine Commands ──────────────────────────────

    case "slope": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/primer`), slug);
      break;
    }

    case "primer": {
      const slug = requireArg(1, "slug");
      const budget = flags.budget;
      const path = budget
        ? `/pyramid/${enc(slug)}/primer/formatted?budget=${enc(budget)}`
        : `/pyramid/${enc(slug)}/primer/formatted`;
      output(await pf(path), slug);
      break;
    }

    case "memoir": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/reading/memoir`), slug);
      break;
    }

    case "walk": {
      const slug = requireArg(1, "slug");
      const params: string[] = [];
      if (flags.layer) params.push(`layer=${enc(flags.layer)}`);
      if (flags.direction) params.push(`direction=${enc(flags.direction)}`);
      if (flags.limit) params.push(`limit=${enc(flags.limit)}`);
      const qs = params.length > 0 ? `?${params.join("&")}` : "";
      output(await pf(`/pyramid/${enc(slug)}/reading/walk${qs}`), slug);
      break;
    }

    case "thread": {
      const slug = requireArg(1, "slug");
      const identity = requireArg(2, "identity");
      output(await pf(`/pyramid/${enc(slug)}/reading/thread?identity=${enc(identity)}`), slug);
      break;
    }

    case "decisions": {
      const slug = requireArg(1, "slug");
      const stance = flags.stance;
      const path = stance
        ? `/pyramid/${enc(slug)}/reading/decisions?stance=${enc(stance)}`
        : `/pyramid/${enc(slug)}/reading/decisions`;
      output(await pf(path), slug);
      break;
    }

    case "speaker": {
      const slug = requireArg(1, "slug");
      const role = requireArg(2, "role");
      output(await pf(`/pyramid/${enc(slug)}/reading/speaker?role=${enc(role)}`), slug);
      break;
    }

    case "reading-search": {
      const slug = requireArg(1, "slug");
      const query = requireArg(2, "query");
      output(await pf(`/pyramid/${enc(slug)}/reading/search?q=${enc(query)}`), slug);
      break;
    }

    case "cold-start": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/manifest/cold-start`), slug);
      break;
    }

    case "manifest": {
      const slug = requireArg(1, "slug");
      const opsJson = requireArg(2, "operations-json");
      let operations: unknown;
      try {
        operations = JSON.parse(opsJson);
      } catch {
        process.stderr.write("Error: operations must be valid JSON\n");
        process.exit(1);
      }
      output(await pf(`/pyramid/${enc(slug)}/manifest`, { method: "POST", body: operations }), slug);
      break;
    }

    case "vocab": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vocabulary`), slug);
      break;
    }

    case "vocab-recognize": {
      const slug = requireArg(1, "slug");
      const term = requireArg(2, "term");
      output(await pf(`/pyramid/${enc(slug)}/vocabulary/recognize?term=${enc(term)}`), slug);
      break;
    }

    case "vocab-diff": {
      const slug = requireArg(1, "slug");
      const since = requireArg(2, "since");
      output(await pf(`/pyramid/${enc(slug)}/vocabulary/diff?since=${enc(since)}`), slug);
      break;
    }

    case "dadbear-status": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/dadbear/status`), slug);
      break;
    }

    case "dadbear-trigger": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/dadbear/trigger`, { method: "POST" }), slug);
      break;
    }

    case "vine-bedrocks": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/bedrocks`), slug);
      break;
    }

    case "vine-add": {
      const slug = requireArg(1, "slug");
      const bedrockSlug = requireArg(2, "bedrock-slug");
      output(await pf(`/pyramid/${enc(slug)}/vine/add-bedrock`, { method: "POST", body: { bedrock_slug: bedrockSlug } }), slug);
      break;
    }

    case "preview": {
      const slug = requireArg(1, "slug");
      const sourcePath = requireArg(2, "source-path");
      const contentType = requireArg(3, "content-type");
      const body: Record<string, string> = { source_path: sourcePath, content_type: contentType };
      if (flags.chain) body.chain = flags.chain;
      output(await pf(`/pyramid/${enc(slug)}/preview`, { method: "POST", body }), slug);
      break;
    }

    case "recovery-status": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/recovery/status`), slug);
      break;
    }

    case "ask": {
      const slug = requireArg(1, "slug");
      const question = requireArg(2, "question");
      const body: Record<string, unknown> = { question };
      if (flags["demand-gen"] === "true") body.demand_gen = true;
      output(await pf(`/pyramid/${enc(slug)}/question`, { method: "POST", body }), slug);
      break;
    }

    case "demand-gen-status": {
      const slug = requireArg(1, "slug");
      const jobId = requireArg(2, "job-id");
      output(await pf(`/pyramid/${enc(slug)}/demand-gen/${enc(jobId)}`), slug);
      break;
    }

    default: {
      process.stderr.write(`Unknown command: ${command}\n\n`);
      printUsage();
      process.exit(1);
    }
  }
}

function printUsage(): void {
  process.stderr.write(`pyramid-cli — Knowledge Pyramid CLI for agent access

Default output is pretty-printed JSON. Use --compact for minified JSON.

Core Commands:
  health                          Check if Wire Node is running
  slugs                           List available pyramids

Exploration Commands:
  apex <slug> [--summary]         Get top-level summary (--summary for stripped version)
  search <slug> <query> [--semantic] Search pyramid nodes (--semantic for LLM fallback)
  drill <slug> <node_id>          Drill into a node + children (enriched with annotations + breadcrumb)
  node <slug> <node_id>           Get a single node
  faq <slug> [query]              Match FAQ or list all
  faq-dir <slug>                  FAQ directory (flat or hierarchical)
  annotations <slug> [node_id]    List annotations (optionally for a specific node)
  annotate <slug> <node_id> <content>  Add annotation

Question Pyramid Commands:
  create-question-slug <name> --ref <slug1> [--ref <slug2>]  Create question slug with references
  question-build <slug> "<question>" [--granularity N] [--max-depth N]  Build question pyramid
  references <slug>                     Show what a slug references and who references it
  composed <slug>                       Get composed view across slug + referenced slugs

Primer/Slope Commands:
  slope <slug>                         Display slope nodes from primer
  primer <slug> [--budget N]           Formatted primer for onboarding

Reading Mode Commands:
  memoir <slug>                        Memoir reading mode (narrative summary)
  walk <slug> [--layer N] [--direction newest|oldest] [--limit N]  Walk through content
  thread <slug> <identity>             Follow an identity's contributions
  decisions <slug> [--stance X]        Extract decision points
  speaker <slug> <role>                View contributions by speaker role
  reading-search <slug> <query>        Search within reading content

Manifest/Runtime Commands:
  cold-start <slug>                    Get cold-start manifest bundle
  manifest <slug> <operations-json>    Execute manifest operations (POST)

Vocabulary Commands:
  vocab <slug>                         Get full vocabulary
  vocab-recognize <slug> <term>        Check if a term is recognized
  vocab-diff <slug> <since>            Vocabulary changes since a point in time

Vine Commands:
  vine-build <slug> <dir1> [dir2...]   Build vine from JSONL directories
  vine-bunches <slug>                  List all bunches with metadata
  vine-eras <slug>                     List ERA annotations
  vine-decisions <slug>                List decision FAQ entries
  vine-entities <slug>                 List entity resolution FAQ entries
  vine-threads <slug>                  List vine thread continuity + web edges
  vine-drill <slug>                    Directory-wired drill (navigation structure)
  vine-rebuild-upper <slug>            Force rebuild L2+ layers
  vine-integrity <slug>                Run integrity check, return results
  vine-bedrocks <slug>                 List bedrock slugs in vine
  vine-add <slug> <bedrock-slug>       Add bedrock slug to vine

Preview Commands:
  preview <slug> <source-path> <content-type> [--chain X]  Dry-run content processing

Recovery Commands:
  recovery-status <slug>               Get recovery status

Analysis Commands:
  entities <slug>                 Entity index
  terms <slug>                    Terms dictionary
  corrections <slug>              Correction log
  edges <slug>                    Web edges graph
  threads <slug>                  Thread clusters
  meta <slug>                     Meta analysis nodes
  resolved <slug>                 Resolution status

Operations Commands:
  tree <slug>                     Structural overview
  dadbear <slug>                  DADBEAR auto-update status (legacy)
  dadbear-status <slug>           DADBEAR status (v2, detailed)
  dadbear-trigger <slug>          Trigger DADBEAR auto-update check
  cost <slug> [--build ID]        Build cost report
  stale-log <slug> [--limit N]    Staleness history
  usage <slug> [--limit N]        Access patterns (default limit=100)
  diff <slug>                     Changelog approximation (stale-log + build status)

Composite Commands:
  handoff <slug>                  Composite handoff block (apex + FAQ + annotations + DADBEAR)
  compare <slug1> <slug2>         Cross-pyramid comparison (terms, children, decisions)
  navigate <slug> "<question>"    One-shot question answering with provenance

Question Commands:
  ask <slug> "<question>" [--demand-gen]  Ask a question (optionally trigger demand gen)
  demand-gen-status <slug> <job-id>       Check demand generation job status

Agent Coordination:
  react <slug> <annotation_id> up|down  Vote on an annotation
  session-register <slug> [--agent name]  Register an agent session
  sessions <slug>                List recent agent sessions

Annotation flags:
  --question "..."     Question this answers (triggers FAQ)
  --author "..."       Your agent name
  --type <type>        observation | correction | question | friction | idea

Options:
  --pretty             Pretty-print JSON output (default: on)
  --compact            Compact JSON output (minified)
  --verbose            Print auth method and diagnostics to stderr
  --help               Show help (use <command> --help for per-command help)

Examples:
  pyramid-cli apex <your-slug>
  pyramid-cli apex <your-slug> --summary
  pyramid-cli search <your-slug> "stale engine"
  pyramid-cli drill <your-slug> C-L0-071
  pyramid-cli faq <your-slug> "How does the stale engine work?"
  pyramid-cli annotate <your-slug> C-L0-071 "Finding text" --question "Q?" --author "my-agent"
  pyramid-cli tree <your-slug>
  pyramid-cli handoff <your-slug>
  pyramid-cli compare slug-one slug-two
  pyramid-cli vine-build my-vine /path/to/jsonl-dir1 /path/to/jsonl-dir2
  pyramid-cli vine-bunches my-vine
  pyramid-cli primer <your-slug>
  pyramid-cli memoir <your-slug>
  pyramid-cli walk <your-slug> --layer 1 --direction oldest --limit 10
  pyramid-cli cold-start <your-slug>
  pyramid-cli vocab <your-slug>
  pyramid-cli ask <your-slug> "How does the stale engine work?" --demand-gen
`);
}

// ── Main ─────────────────────────────────────────────────────────────────────

run().catch((err: unknown) => {
  process.stderr.write(
    `Fatal: ${err instanceof Error ? err.message : String(err)}\n`
  );
  process.exit(1);
});
