# Pyramid CLI & MCP Cheat Sheet

> Canonical reference for all pyramid operations via CLI, MCP tools, and HTTP endpoints.
> Server: `http://localhost:8765` | Auth: `Bearer <token>` from `PYRAMID_AUTH_TOKEN` env or `~/Library/Application Support/wire-node/pyramid_config.json`

## Full Lifecycle: Create → Ingest → Build → Read

```bash
# 1. Create a slug
pyramid-cli create-slug my-project --type code --source /path/to/src

# 2. Ingest source files (scan + chunk)
pyramid-cli ingest my-project

# 3. Trigger build (async — returns immediately)
pyramid-cli build my-project

# 4. Poll until done
pyramid-cli build-status my-project

# 5. Read the result
pyramid-cli apex my-project
```

---

## CLI Commands

### Running the CLI

```bash
# From repo root:
node mcp-server/dist/cli.js <command> [args] [--flags]

# Or with alias:
alias pyramid-cli='node /path/to/agent-wire-node/mcp-server/dist/cli.js'
```

### Read Operations

| Command | Description |
|---|---|
| `health` | Check Wire Node server status |
| `slugs` | List all available pyramids |
| `apex <slug>` | Top-level summary (the apex node) |
| `search <slug> <query>` | Search nodes by natural language |
| `drill <slug> <node_id>` | Node + its children |
| `node <slug> <node_id>` | Single node only |
| `faq <slug> [query]` | Match FAQ or list all |
| `faq-dir <slug>` | FAQ directory (hierarchical) |
| `annotations <slug> [node_id]` | List annotations |

### Mutation Operations

| Command | Description |
|---|---|
| `create-slug <name> --type <type> --source /path` | Create pyramid (code, document, conversation) |
| `ingest <slug>` | Scan + chunk source files |
| `build <slug> [--from-depth N]` | Trigger full build (async) |
| `build-status <slug>` | Poll build progress |

### Write Operations

| Command | Description |
|---|---|
| `annotate <slug> <node_id> <content>` | Add annotation to a node |

Annotation flags: `--question "..."` `--author "..."` `--type observation|correction|question|friction|idea`

### Question Pyramid Commands

| Command | Description |
|---|---|
| `create-question-slug <name> --ref <slug1> [--ref <slug2>]` | Create cross-pyramid question slug |
| `question-build <slug> "<question>" [--granularity N] [--max-depth N]` | Build question pyramid |
| `references <slug>` | Show reference graph |
| `composed <slug>` | Composed view across referenced slugs |

### Vine (Conversation) Commands

| Command | Description |
|---|---|
| `vine-build <slug> <dir1> [dir2...]` | Build vine from JSONL dirs |
| `vine-bunches <slug>` | List bunches with metadata |
| `vine-eras <slug>` | List ERA annotations |
| `vine-decisions <slug>` | List decision FAQ entries |
| `vine-entities <slug>` | List entity resolution FAQ |
| `vine-threads <slug>` | Thread continuity + web edges |
| `vine-drill <slug>` | Directory-wired navigation |
| `vine-rebuild-upper <slug>` | Force rebuild L2+ |
| `vine-integrity <slug>` | Run integrity check |

### Global Flags

| Flag | Description |
|---|---|
| `--pretty` | Pretty-print JSON (default) |
| `--compact` | Compact JSON output |

---

## MCP Tools

All 17 tools are available via the `wire-node-pyramid` MCP server (`mcp-server/dist/index.js`, stdio transport).

### Read Tools

| Tool | Parameters | Description |
|---|---|---|
| `pyramid_health` | — | Server health check |
| `pyramid_list_slugs` | — | List all pyramids |
| `pyramid_apex` | `slug` | Apex node |
| `pyramid_search` | `slug`, `query` | Search nodes |
| `pyramid_drill` | `slug`, `node_id` | Drill into node |
| `pyramid_faq_match` | `slug`, `query` | Match FAQ |
| `pyramid_faq_directory` | `slug` | FAQ directory |
| `pyramid_references` | `slug` | Reference graph |
| `pyramid_composed_view` | `slug` | Composed cross-slug view |

### Mutation Tools

| Tool | Parameters | Description |
|---|---|---|
| `pyramid_create_slug` | `slug`, `content_type`, `source_path` | Create new pyramid |
| `pyramid_ingest` | `slug` | Ingest source files (5m timeout) |
| `pyramid_build` | `slug`, `from_depth?` | Trigger async build (5m timeout) |
| `pyramid_build_status` | `slug` | Poll build progress |
| `pyramid_build_cancel` | `slug` | Cancel running build |

### Write Tools

| Tool | Parameters | Description |
|---|---|---|
| `pyramid_annotate` | `slug`, `node_id`, `content`, `question_context?`, `annotation_type?`, `author?` | Add annotation |

### Question Pyramid Tools

| Tool | Parameters | Description |
|---|---|---|
| `pyramid_create_question_slug` | `slug`, `referenced_slugs[]` | Create question slug |
| `pyramid_question_build` | `slug`, `question`, `granularity?`, `max_depth?` | Build question pyramid |

---

## HTTP Endpoints (curl)

### Read (always available)

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"

curl -s -H "$AUTH" localhost:8765/health
curl -s -H "$AUTH" localhost:8765/pyramid/slugs
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/apex
curl -s -H "$AUTH" "localhost:8765/pyramid/<slug>/search?q=<query>"
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/drill/<node_id>
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/node/<node_id>
curl -s -H "$AUTH" "localhost:8765/pyramid/<slug>/faq/match?q=<query>"
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/faq/directory
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/annotations
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/build/status
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/references
curl -s -H "$AUTH" localhost:8765/pyramid/<slug>/composed
```

### Mutations (localhost only, auth required)

```bash
# Create slug
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/slugs \
  -d '{"slug":"my-proj","content_type":"code","source_path":"/abs/path"}'

# Ingest
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/<slug>/ingest

# Build (async — returns immediately with status "running")
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/<slug>/build

# Build from specific depth
curl -s -H "$AUTH" -X POST "localhost:8765/pyramid/<slug>/build?from_depth=1"

# Cancel build
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/<slug>/build/cancel

# Annotate
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/<slug>/annotate \
  -d '{"node_id":"L1-003","content":"Finding text","author":"my-agent","annotation_type":"observation"}'
```

### IPC-Only (desktop UI only)

These operations remain behind Tauri IPC and cannot be called via HTTP or CLI:

- `pyramid_delete_slug` — destructive, UI-gated
- `pyramid_set_config` — credential write
- `pyramid_archive_slug` / `pyramid_purge_slug` — destructive
- `pyramid_auto_update_*` — DADBEAR configuration
- `pyramid_breaker_*` — circuit breaker controls
- `pyramid_crystallize` — finalization

---

## Known Pyramid Slugs

| Slug | Type | Description |
|---|---|---|
| `opt-025` | code | agent-wire-node codebase |
| `goodnewseveryone` | code | GoodNewsEveryone codebase |
| `core-selected-docs` | document | All project design docs |

---

## Common Patterns

### Experiment workflow (prompt tuning)

```bash
# Create experiment slug
pyramid-cli create-slug vibesmithy-exp1 --type code --source /path/to/vibesmithy

# Ingest + build
pyramid-cli ingest vibesmithy-exp1
pyramid-cli build vibesmithy-exp1

# Poll until complete
while true; do
  status=$(pyramid-cli build-status vibesmithy-exp1 --compact | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
  echo "$status"
  [ "$status" != "running" ] && break
  sleep 10
done

# Evaluate
pyramid-cli apex vibesmithy-exp1
pyramid-cli drill vibesmithy-exp1 L2-000
```

### Agent annotation loop

```bash
# Annotate while working
pyramid-cli annotate opt-025 L1-003 "Found race condition in build runner" \
  --question "Is the build runner thread-safe?" \
  --author "auditor-1" \
  --type correction

# Check it landed
pyramid-cli annotations opt-025 L1-003
```

### Question pyramid (cross-pyramid query)

```bash
pyramid-cli create-question-slug auth-analysis --ref opt-025 --ref goodnewseveryone
pyramid-cli question-build auth-analysis "How does authentication flow between the node and the Wire?"
pyramid-cli composed auth-analysis
```
