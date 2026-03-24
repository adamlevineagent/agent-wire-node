#!/usr/bin/env node

/**
 * pyramid-cli — Knowledge Pyramid CLI for agent access
 *
 * Usage:
 *   node dist/cli.js <command> [args] [--compact] [--flag value]
 *
 * Commands:
 *   health                          Check if Wire Node is running
 *   slugs                           List available pyramids
 *   apex <slug>                     Get top-level summary
 *   search <slug> <query>           Search pyramid nodes
 *   drill <slug> <node_id>          Drill into a node + children
 *   node <slug> <node_id>           Get a single node
 *   faq <slug> [query]              Match FAQ or list all
 *   faq-dir <slug>                  FAQ directory (flat or hierarchical)
 *   annotations <slug> [node_id]    List annotations (optionally for a specific node)
 *   annotate <slug> <node_id> <content>  Add annotation
 *
 * Annotation flags:
 *   --question "..."     Question this answers (triggers FAQ)
 *   --author "..."       Your agent name
 *   --type <type>        observation | correction | question | friction | idea
 *
 * Options:
 *   --pretty             Pretty-print JSON output (default: on)
 *   --compact            Compact JSON output
 */

import { resolveAuthToken, pyramidFetch, type PyramidResponse } from "./lib.js";

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

// ── Auth ─────────────────────────────────────────────────────────────────────

const AUTH_TOKEN = resolveAuthToken();

/** Shorthand that injects the auth token. */
function pf(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<PyramidResponse> {
  return pyramidFetch(path, { ...options, authToken: AUTH_TOKEN });
}

// ── Output ───────────────────────────────────────────────────────────────────

function output(resp: PyramidResponse): void {
  if (!resp.ok) {
    const payload =
      typeof resp.data === "object" && resp.data !== null
        ? resp.data
        : { error: String(resp.data), status: resp.status };
    process.stderr.write(JSON.stringify(payload, null, 2) + "\n");
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
  if (!command || flags.help === "true") {
    printUsage();
    if (!command) process.exit(1);
    process.exit(0);
  }

  switch (command) {
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
      output(await pf(`/pyramid/${enc(slug)}/apex`));
      break;
    }

    case "search": {
      const slug = requireArg(1, "slug");
      const query = requireArg(2, "query");
      output(await pf(`/pyramid/${enc(slug)}/search?q=${enc(query)}`));
      break;
    }

    case "drill": {
      const slug = requireArg(1, "slug");
      const nodeId = requireArg(2, "node_id");
      output(await pf(`/pyramid/${enc(slug)}/drill/${enc(nodeId)}`));
      break;
    }

    case "node": {
      const slug = requireArg(1, "slug");
      const nodeId = requireArg(2, "node_id");
      output(await pf(`/pyramid/${enc(slug)}/node/${enc(nodeId)}`));
      break;
    }

    case "faq": {
      const slug = requireArg(1, "slug");
      const query = positional[2]; // optional
      if (query) {
        const resp = await pf(`/pyramid/${enc(slug)}/faq/match?q=${enc(query)}`);
        // Fix #5: handle null/empty FAQ response
        if (resp.ok && (resp.data === null || resp.data === undefined)) {
          outputData({ matches: [], message: "No FAQ entries matched your query." });
        } else {
          output(resp);
        }
      } else {
        output(await pf(`/pyramid/${enc(slug)}/faq/directory`));
      }
      break;
    }

    case "faq-dir": {
      const slug = requireArg(1, "slug");
      output(await pf(`/pyramid/${enc(slug)}/faq/directory`));
      break;
    }

    case "annotations": {
      const slug = requireArg(1, "slug");
      const nodeId = positional[2]; // optional
      const path = nodeId
        ? `/pyramid/${enc(slug)}/annotations?node_id=${enc(nodeId)}`
        : `/pyramid/${enc(slug)}/annotations`;
      output(await pf(path));
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
        output(resp);
      }
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

Commands:
  health                          Check if Wire Node is running
  slugs                           List available pyramids
  apex <slug>                     Get top-level summary
  search <slug> <query>           Search pyramid nodes
  drill <slug> <node_id>          Drill into a node + children
  node <slug> <node_id>           Get a single node
  faq <slug> [query]              Match FAQ or list all
  faq-dir <slug>                  FAQ directory (flat or hierarchical)
  annotations <slug> [node_id]    List annotations (optionally for a specific node)
  annotate <slug> <node_id> <content>  Add annotation

Annotation flags:
  --question "..."     Question this answers (triggers FAQ)
  --author "..."       Your agent name
  --type <type>        observation | correction | question | friction | idea

Options:
  --pretty             Pretty-print JSON output (default: on)
  --compact            Compact JSON output

Examples:
  pyramid-cli apex agent-wire-nodepostdadbear
  pyramid-cli search agent-wire-nodepostdadbear "stale engine"
  pyramid-cli annotate agent-wire-nodepostdadbear C-L0-071 "Finding text" --question "Q?" --author "my-agent"
`);
}

// ── Main ─────────────────────────────────────────────────────────────────────

run().catch((err: unknown) => {
  process.stderr.write(
    `Fatal: ${err instanceof Error ? err.message : String(err)}\n`
  );
  process.exit(1);
});
