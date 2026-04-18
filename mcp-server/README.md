# Agent Wire Node Pyramid -- CLI & MCP Server

> Agent-facing interface to Knowledge Pyramids. Navigate, search, annotate, and compose structured intelligence.

Both interfaces (`pyramid-cli` and the MCP server) are thin HTTP clients that talk to the Agent Wire Node Rust backend at `localhost:8765`. The CLI outputs JSON to stdout. The MCP server exposes the same capabilities over stdio using the Model Context Protocol.

Package: `wire-node-pyramid-mcp` v0.2.0

---

## Quick Start

### CLI

```bash
# Build
cd mcp-server && npm install && npm run build

# Verify connectivity
pyramid-cli health

# List available pyramids
pyramid-cli slugs

# Get a structural overview
pyramid-cli apex my-pyramid --summary

# Search
pyramid-cli search my-pyramid "stale engine"

# Drill into a node
pyramid-cli drill my-pyramid L1-003
```

The CLI binary is `dist/cli.js`, registered as `pyramid-cli` in package.json.

### MCP Server

Add to your Claude Desktop config (`~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "wire-node-pyramid": {
      "command": "node",
      "args": ["/absolute/path/to/mcp-server/dist/index.js"],
      "env": {
        "PYRAMID_AUTH_TOKEN": "your-token-here"
      }
    }
  }
}
```

The MCP server runs on stdio transport. On startup it performs a non-blocking connectivity check against Agent Wire Node and logs the result to stderr. If Agent Wire Node is not running, tools will return clear error messages until it comes online.

---

## Authentication

The auth token is resolved in this order:

1. **Environment variable** `PYRAMID_AUTH_TOKEN` (highest priority)
2. **Config file** `~/Library/Application Support/wire-node/pyramid_config.json` -- must contain an `auth_token` string field

```json
{
  "auth_token": "your-token-here"
}
```

If neither source provides a token, the process exits with a fatal error.

Use `--verbose` with the CLI to print which auth source was used:

```bash
pyramid-cli health --verbose
# stderr: [verbose] Auth resolved via env:PYRAMID_AUTH_TOKEN
```

---

## Commands Reference

### Core

#### health

Check if Agent Wire Node is running. Returns server version and status.

```
pyramid-cli health
```

MCP tool: `pyramid_health`

```bash
pyramid-cli health
```

---

#### slugs

List all available pyramid slugs with content types and metadata.

```
pyramid-cli slugs
```

MCP tool: `pyramid_list_slugs`

```bash
pyramid-cli slugs
```

---

#### help

Self-documenting help system. Returns the full tool catalog as structured JSON.

```
pyramid-cli help [command] [--category <category>]
```

MCP tool: `pyramid_help`

```bash
pyramid-cli help
pyramid-cli help drill
pyramid-cli help --category exploration
```

---

### Exploration

#### apex

Get the apex (top-level) node -- the system overview with headline, terms, children, and structural summary.

```
pyramid-cli apex <slug> [--summary]
```

MCP tool: `pyramid_apex` (pass `summary_only: true` for the stripped version)

The `--summary` flag strips the response to: headline, distilled, self_prompt, children IDs, and terms only. Use this for token-efficient onboarding.

```bash
pyramid-cli apex my-pyramid
pyramid-cli apex my-pyramid --summary
```

---

#### search

Full-text keyword search across pyramid nodes. Results ranked by depth (L3>L2>L1>L0) then by query term frequency.

```
pyramid-cli search <slug> <query> [--semantic]
```

MCP tool: `pyramid_search`

The `--semantic` flag enables LLM-backed keyword rewriting when FTS returns 0 results. Costs 1 LLM call on fallback.

Returns `_hint` on 0 results suggesting FAQ.

```bash
pyramid-cli search my-pyramid "stale engine"
pyramid-cli search my-pyramid "stale engine" --semantic
```

---

#### drill

Drill into a specific node: returns the node, its children, evidence, gaps, and question_context. Enriched with inline annotations and a breadcrumb trail from apex to current node.

```
pyramid-cli drill <slug> <node_id>
```

MCP tool: `pyramid_drill`

Node IDs follow the format `L2-000`, `L1-003`, `L0-012`. For question pyramids, cross-slug references use `source-slug:L1-003`.

```bash
pyramid-cli drill my-pyramid L1-003
```

---

#### node

Get a single node by ID without children or evidence. Use `drill` instead if you need the full context.

```
pyramid-cli node <slug> <node_id>
```

MCP tool: `pyramid_node`

```bash
pyramid-cli node my-pyramid L0-012
```

---

#### faq

Match a natural-language question against FAQ entries. Without a query, lists all FAQ entries.

```
pyramid-cli faq <slug> [query]
```

MCP tool: `pyramid_faq_match`

Returns `_hint` on 0 matches suggesting search.

```bash
pyramid-cli faq my-pyramid "How does the stale engine work?"
pyramid-cli faq my-pyramid
```

---

#### faq-dir

FAQ directory listing. Same as `faq` without a query. Shows all FAQ entries organized by category.

```
pyramid-cli faq-dir <slug>
```

MCP tool: `pyramid_faq_directory`

```bash
pyramid-cli faq-dir my-pyramid
```

---

#### tree

Full tree structure in one call. Returns all nodes as an indented hierarchy (L3 > L2 > L1 > L0) with headline and child count per node.

```
pyramid-cli tree <slug>
```

MCP tool: `pyramid_tree`

```bash
pyramid-cli tree my-pyramid
```

---

#### navigate

One-shot question answering. Searches for relevant nodes, fetches content, and synthesizes a direct answer with provenance citations. Costs 1 LLM call.

```
pyramid-cli navigate <slug> "<question>"
```

MCP tool: `pyramid_navigate`

```bash
pyramid-cli navigate my-pyramid "How does the stale engine work?"
```

---

### Analysis

#### entities

All extracted entities (people, systems, concepts) across the pyramid. Find where any entity is mentioned without searching.

```
pyramid-cli entities <slug>
```

MCP tool: `pyramid_entities`

```bash
pyramid-cli entities my-pyramid
```

---

#### terms

Terms dictionary -- defined vocabulary with definitions. Essential for cold-start onboarding to learn the pyramid's language.

```
pyramid-cli terms <slug>
```

MCP tool: `pyramid_terms`

```bash
pyramid-cli terms my-pyramid
```

---

#### corrections

Correction log -- what was wrong in the source material that the pyramid corrected during build. Quality signal.

```
pyramid-cli corrections <slug>
```

MCP tool: `pyramid_corrections`

```bash
pyramid-cli corrections my-pyramid
```

---

#### edges

Web edges -- all lateral connections between nodes. Cross-cutting themes and relationships without iterative drilling.

```
pyramid-cli edges <slug>
```

MCP tool: `pyramid_edges`

```bash
pyramid-cli edges my-pyramid
```

---

#### threads

Thread clusters showing how L0 nodes were grouped into L1 themes. Reveals the pyramid's organizational logic.

```
pyramid-cli threads <slug>
```

MCP tool: `pyramid_threads`

```bash
pyramid-cli threads my-pyramid
```

---

#### meta

Meta-analysis nodes from post-build passes (webbing, entity resolution). Higher-order structural intelligence.

```
pyramid-cli meta <slug>
```

MCP tool: `pyramid_meta`

```bash
pyramid-cli meta my-pyramid
```

---

#### resolved

Resolution status across the pyramid. Which questions/gaps have been answered and which remain open.

```
pyramid-cli resolved <slug>
```

MCP tool: `pyramid_resolved`

```bash
pyramid-cli resolved my-pyramid
```

---

### Operations

#### dadbear

DADBEAR auto-update status: enabled/disabled, last check, debounce, breaker/freeze state.

```
pyramid-cli dadbear <slug>
```

MCP tool: `pyramid_dadbear_status`

```bash
pyramid-cli dadbear my-pyramid
```

---

#### cost

Token and dollar cost of building this pyramid. Filter by build ID for historical cost.

```
pyramid-cli cost <slug> [--build <build_id>]
```

MCP tool: `pyramid_cost`

```bash
pyramid-cli cost my-pyramid
pyramid-cli cost my-pyramid --build abc123
```

---

#### stale-log

Staleness evaluation history: which nodes were re-evaluated, when, and why. Assess freshness and trust.

```
pyramid-cli stale-log <slug> [--limit N]
```

MCP tool: `pyramid_stale_log`

```bash
pyramid-cli stale-log my-pyramid
pyramid-cli stale-log my-pyramid --limit 20
```

---

#### usage

Access pattern statistics: most frequently accessed nodes. Navigation prioritization signal.

```
pyramid-cli usage <slug> [--limit N]
```

MCP tool: `pyramid_usage`

Default limit: 100.

```bash
pyramid-cli usage my-pyramid
pyramid-cli usage my-pyramid --limit 50
```

---

#### diff

Changelog approximation: stale-log + build status. See what changed since your last visit.

```
pyramid-cli diff <slug>
```

MCP tool: `pyramid_diff`

```bash
pyramid-cli diff my-pyramid
```

---

### Annotations

#### annotations

List annotations. Optionally filter to a specific node. Annotations are agent-contributed knowledge, corrections, and insights.

```
pyramid-cli annotations <slug> [node_id]
```

MCP tool: `pyramid_annotations` (via `pyramid_drill` enrichment; standalone listing not exposed as separate MCP tool)

```bash
pyramid-cli annotations my-pyramid
pyramid-cli annotations my-pyramid L0-012
```

---

#### annotate

Add an annotation to a node. Captures knowledge, corrections, or insights. Annotations with `question_context` trigger FAQ creation.

```
pyramid-cli annotate <slug> <node_id> <content> [--question "..."] [--author "..."] [--type <type>]
```

MCP tool: `pyramid_annotate`

Flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--question` | -- | Question this answers (triggers FAQ creation) |
| `--author` | `cli-agent` | Your agent name |
| `--type` | `observation` | One of: `observation`, `correction`, `question`, `friction`, `idea` |

```bash
pyramid-cli annotate my-pyramid L0-012 "The retry logic caps at 3 attempts" --question "How many retries?" --author auditor-1 --type observation
```

---

#### react

Vote on an annotation (up/down). Each agent can vote once per annotation. Subsequent votes replace the previous one.

```
pyramid-cli react <slug> <annotation_id> up|down [--agent <name>]
```

MCP tool: `pyramid_react`

```bash
pyramid-cli react my-pyramid 42 up --agent my-agent
```

---

### Composite

#### handoff

Generate a complete onboarding handoff block. Fetches apex, FAQ, annotations, and DADBEAR status in parallel. Returns CLI command templates, annotation summary, top FAQ questions, and tips.

```
pyramid-cli handoff <slug>
```

MCP tool: `pyramid_handoff`

```bash
pyramid-cli handoff my-pyramid
```

---

#### compare

Cross-pyramid comparison: shared/unique terms, conflicting definitions, structural differences, decision counts.

```
pyramid-cli compare <slug1> <slug2>
```

MCP tool: `pyramid_compare`

```bash
pyramid-cli compare pyramid-a pyramid-b
```

---

### Agent Coordination

#### session-register

Register an agent session on a pyramid. Other agents can see active sessions.

```
pyramid-cli session-register <slug> [--agent <name>]
```

MCP tool: `pyramid_session_register`

Default agent name: `cli-agent`.

```bash
pyramid-cli session-register my-pyramid --agent auditor-1
```

---

#### sessions

List recent agent sessions. Shows which agents have been exploring, when they were last active, and how many actions they took.

```
pyramid-cli sessions <slug>
```

MCP tool: `pyramid_sessions`

```bash
pyramid-cli sessions my-pyramid
```

---

### Question Pyramids

#### create-question-slug

Create a question pyramid slug that references one or more source pyramids. Question slugs compose knowledge across references.

```
pyramid-cli create-question-slug <name> --ref <slug1> [--ref <slug2> ...]
```

MCP tool: `pyramid_create_question_slug`

At least one `--ref` is required. The flag is repeatable.

```bash
pyramid-cli create-question-slug my-question --ref source-1 --ref source-2
```

---

#### question-build

Build a question pyramid: decomposes the question into sub-questions and builds answer nodes across referenced source pyramids.

```
pyramid-cli question-build <slug> "<question>" [--granularity N] [--max-depth N]
```

MCP tool: `pyramid_question_build`

| Flag | Default | Description |
|------|---------|-------------|
| `--granularity` | 3 | Sub-questions per decomposition level |
| `--max-depth` | 3 | Maximum decomposition depth |

```bash
pyramid-cli question-build my-question "How do the three systems coordinate failure recovery?"
```

---

#### references

Show the reference graph: what this slug references and what references it.

```
pyramid-cli references <slug>
```

MCP tool: `pyramid_references`

```bash
pyramid-cli references my-question
```

---

#### composed

Composed view across a question slug and all its referenced source pyramids. Shows all nodes and edges.

```
pyramid-cli composed <slug>
```

MCP tool: `pyramid_composed_view`

```bash
pyramid-cli composed my-question
```

---

### Vine Conversations

Vine commands process JSONL conversation directories into structured knowledge. Vine commands are CLI-only (no MCP tool equivalents).

#### vine-build

Build a vine from JSONL conversation directories.

```
pyramid-cli vine-build <slug> <dir1> [dir2 ...]
```

```bash
pyramid-cli vine-build my-vine /path/to/jsonl-dir1 /path/to/jsonl-dir2
```

---

#### vine-bunches

List all bunches (conversation groups) with metadata.

```
pyramid-cli vine-bunches <slug>
```

```bash
pyramid-cli vine-bunches my-vine
```

---

#### vine-eras

List ERA (event-response-action) annotations across the vine.

```
pyramid-cli vine-eras <slug>
```

---

#### vine-decisions

List decision FAQ entries extracted from conversations.

```
pyramid-cli vine-decisions <slug>
```

---

#### vine-entities

List entity resolution FAQ entries from conversations.

```
pyramid-cli vine-entities <slug>
```

---

#### vine-threads

List vine thread continuity and web edges between conversation segments.

```
pyramid-cli vine-threads <slug>
```

---

#### vine-drill

Directory-wired drill for vine navigation structure.

```
pyramid-cli vine-drill <slug>
```

---

#### vine-rebuild-upper

Force rebuild of L2+ layers for a vine.

```
pyramid-cli vine-rebuild-upper <slug>
```

---

#### vine-integrity

Run integrity check on a vine, return validation results.

```
pyramid-cli vine-integrity <slug>
```

---

## MCP Tools Reference

| CLI Command | MCP Tool Name | Category |
|---|---|---|
| `health` | `pyramid_health` | core |
| `slugs` | `pyramid_list_slugs` | core |
| `help` | `pyramid_help` | core |
| `apex` | `pyramid_apex` | exploration |
| `search` | `pyramid_search` | exploration |
| `drill` | `pyramid_drill` | exploration |
| `node` | `pyramid_node` | exploration |
| `faq` | `pyramid_faq_match` | exploration |
| `faq-dir` | `pyramid_faq_directory` | exploration |
| `tree` | `pyramid_tree` | exploration |
| `navigate` | `pyramid_navigate` | exploration |
| `entities` | `pyramid_entities` | analysis |
| `terms` | `pyramid_terms` | analysis |
| `corrections` | `pyramid_corrections` | analysis |
| `edges` | `pyramid_edges` | analysis |
| `threads` | `pyramid_threads` | analysis |
| `meta` | `pyramid_meta` | analysis |
| `resolved` | `pyramid_resolved` | analysis |
| `dadbear` | `pyramid_dadbear_status` | operations |
| `cost` | `pyramid_cost` | operations |
| `stale-log` | `pyramid_stale_log` | operations |
| `usage` | `pyramid_usage` | operations |
| `diff` | `pyramid_diff` | operations |
| `annotations` | -- | annotation |
| `annotate` | `pyramid_annotate` | annotation |
| `react` | `pyramid_react` | annotation |
| `handoff` | `pyramid_handoff` | composite |
| `compare` | `pyramid_compare` | composite |
| `create-question-slug` | `pyramid_create_question_slug` | question |
| `question-build` | `pyramid_question_build` | question |
| `references` | `pyramid_references` | question |
| `composed` | `pyramid_composed_view` | question |
| `session-register` | `pyramid_session_register` | coordination |
| `sessions` | `pyramid_sessions` | coordination |
| `vine-*` | -- (CLI only) | vine |

---

## Flags & Options

### Output Flags

| Flag | Description |
|------|-------------|
| `--pretty` | Pretty-print JSON output (default: on) |
| `--compact` | Compact/minified JSON output |
| `--verbose` | Print auth method and diagnostics to stderr |
| `--help` / `-h` | Show help. Use `<command> --help` for per-command help |

### Command-Specific Flags

| Flag | Used By | Description |
|------|---------|-------------|
| `--summary` | `apex` | Strip to headline, distilled, self_prompt, children IDs, terms |
| `--semantic` | `search` | Enable LLM keyword rewriting fallback on 0 FTS results |
| `--limit N` | `stale-log`, `usage` | Max entries to return |
| `--build ID` | `cost` | Filter to a specific build ID |
| `--question "..."` | `annotate` | Question this answers (triggers FAQ creation) |
| `--author "..."` | `annotate` | Agent name (default: `cli-agent`) |
| `--type <type>` | `annotate` | `observation` / `correction` / `question` / `friction` / `idea` |
| `--agent <name>` | `react`, `session-register` | Agent identifier |
| `--ref <slug>` | `create-question-slug` | Source slug to reference (repeatable) |
| `--granularity N` | `question-build` | Sub-questions per decomposition level |
| `--max-depth N` | `question-build` | Maximum decomposition depth |
| `--category <cat>` | `help` | Filter help to a category |

---

## Response Enrichments

Several commands enrich raw backend responses with additional context:

- **drill** -- Injects inline annotations for the drilled node and builds a breadcrumb trail (array of `{id, headline, depth}` from apex to current node) by walking `parent_id` up to 5 levels.
- **search** -- Client-side re-ranking by query term frequency in snippets. Returns `_hint` on 0 results suggesting FAQ.
- **faq** -- Returns `_hint` on 0 matches suggesting search.
- **apex --summary** -- Strips response to `headline`, `distilled`, `self_prompt`, `children` (IDs only), and `terms`.
- **annotate** -- Returns `_note` confirming save and integration behavior (FAQ processing for `question_context` runs in background).
- **faq-dir** -- Appends `_note` explaining relationship to `faq` without query.
- **Error responses** -- Inject `_hint` with actionable suggestions (e.g., "Run 'pyramid-cli slugs' to see available pyramids").

---

## Self-Documenting Help

The tool catalog is built into the package at `lib.ts` and is queryable at runtime:

```bash
# Full catalog as structured JSON
pyramid-cli help

# Help for a specific command
pyramid-cli help drill

# Filter by category
pyramid-cli help --category exploration
pyramid-cli help --category analysis
```

Available categories: `core`, `exploration`, `analysis`, `operations`, `composite`, `question`, `vine`, `annotation`, `coordination`.

Via MCP: call `pyramid_help` with optional `command` or `category` parameters.

Each catalog entry includes: CLI name, MCP tool name, category, description, args with types, flags, examples, and related commands.

---

## Examples

### Agent Onboarding Flow

Step-by-step for a new agent connecting to an existing pyramid:

```bash
# 1. Get the composite handoff block (apex + FAQ + annotations + DADBEAR)
pyramid-cli handoff my-pyramid

# 2. Read the token-efficient apex summary
pyramid-cli apex my-pyramid --summary

# 3. Learn the vocabulary
pyramid-cli terms my-pyramid

# 4. Search for something specific
pyramid-cli search my-pyramid "retry logic"

# 5. Drill into a result
pyramid-cli drill my-pyramid L0-042

# 6. Leave a finding for future agents
pyramid-cli annotate my-pyramid L0-042 "Retry caps at 3 with exponential backoff" --question "What is the retry strategy?" --author onboarding-agent --type observation
```

### Cold-Start Question Answering

When you have a question and no prior context:

```bash
# Option A: One-shot (uses LLM, costs 1 call)
pyramid-cli navigate my-pyramid "How does the stale engine decide what to rebuild?"

# Option B: Manual (no LLM cost)
pyramid-cli search my-pyramid "stale engine rebuild" --semantic
pyramid-cli drill my-pyramid L1-007
pyramid-cli faq my-pyramid "How does the stale engine work?"
```

### Multi-Agent Coordination

```bash
# Register your session
pyramid-cli session-register my-pyramid --agent auditor-1

# Do your work...
pyramid-cli search my-pyramid "auth flow"
pyramid-cli drill my-pyramid L0-015
pyramid-cli annotate my-pyramid L0-015 "Missing rate limit check" --type correction --author auditor-1

# Check who else is working on this pyramid
pyramid-cli sessions my-pyramid
```

### Cross-Pyramid Analysis

```bash
# Compare two pyramids
pyramid-cli compare codebase-v1 codebase-v2

# Create a question pyramid that spans both
pyramid-cli create-question-slug migration-questions --ref codebase-v1 --ref codebase-v2
pyramid-cli question-build migration-questions "What breaking changes exist between v1 and v2?"
pyramid-cli composed migration-questions
```

---

## Architecture

```
                  CLI (cli.ts)
                      |
                      v
    lib.ts (auth + pyramidFetch + TOOL_CATALOG)
                      |
         HTTP (localhost:8765)
                      |
                      v
           Agent Wire Node (Rust backend)
                      |
                      v
                   SQLite


                MCP Server (index.ts)
                      |
                      v
    lib.ts (auth + pyramidFetch + TOOL_CATALOG)
                      |
         HTTP (localhost:8765)
                      |
                      v
           Agent Wire Node (Rust backend)
```

- **CLI** (`cli.ts`) -- Thin HTTP client. Parses args, calls the Rust backend, enriches responses (breadcrumbs, re-ranking, hints), outputs JSON to stdout.
- **MCP Server** (`index.ts`) -- Same HTTP client wrapped in MCP protocol over stdio. Uses `@modelcontextprotocol/sdk`. Each MCP tool maps to one or more backend endpoints.
- **Shared library** (`lib.ts`) -- Auth token resolution, `pyramidFetch` HTTP helper with timeout and error handling, and the self-documenting `TOOL_CATALOG`.
- **Agent Wire Node** -- Tauri/Rust desktop app that owns the SQLite database, runs builds, manages DADBEAR auto-updates, and serves the HTTP API.

The Agent Wire Node HTTP server must be running for any command to work. The base URL is hardcoded to `http://localhost:8765`. Request timeout is 10 seconds.
