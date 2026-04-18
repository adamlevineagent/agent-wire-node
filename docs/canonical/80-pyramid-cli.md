# `pyramid-cli`

`pyramid-cli` is the agent-facing command-line interface to Wire Node. It's a thin HTTP client over `localhost:8765`, with 64 commands across 16 categories. This is how scripts, external agents, and anyone-not-using-the-Tauri-UI talks to a running Wire Node.

The CLI is also the foundation for the MCP server — every MCP tool maps to a CLI command, so whatever you can do from the CLI, Claude or any MCP-capable agent can do over stdio.

---

## Install and invocation

The CLI ships with Wire Node's repo at `mcp-server/`. Build:

```bash
cd mcp-server && npm install && npm run build
```

Invoke:

```bash
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" <command> [args]
```

Or alias for convenience:

```bash
alias pyramid-cli='node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"'
pyramid-cli health
```

The CLI requires Wire Node to be running (HTTP server at `localhost:8765`).

---

## Authentication

The auth token is resolved in this order:

1. **Env var** `PYRAMID_AUTH_TOKEN` (highest priority).
2. **Config file** `~/Library/Application Support/wire-node/pyramid_config.json` — must have `auth_token` field.

If neither exists, the CLI exits with a fatal error.

Use `--verbose` to see which auth source the CLI used:

```bash
pyramid-cli health --verbose
# stderr: [verbose] Auth resolved via config:pyramid_config.json
```

---

## The 16 categories and representative commands

The CLI's 64 commands are organized by category. Run `pyramid-cli help --category <name>` for details on any category; `pyramid-cli help` returns the full catalog as JSON.

### Core

`health`, `slugs`, `help`

- **`health`** — Wire Node connectivity check. Server version + status.
- **`slugs`** — list all available pyramids with metadata.
- **`help`** — self-documenting. Returns the tool catalog.

### Exploration

`apex`, `search`, `drill`, `node`, `faq`, `faq-dir`, `tree`, `navigate`

- **`apex <slug> [--summary]`** — top-level node. `--summary` strips to headline/distilled/self_prompt/children/terms.
- **`search <slug> <query> [--semantic]`** — FTS across pyramid nodes. `--semantic` falls back to LLM keyword rewriting on 0 results (1 LLM call).
- **`drill <slug> <node_id>`** — single node with children, evidence, gaps, question context, inline annotations, breadcrumb trail.
- **`node <slug> <node_id>`** — bare node without enrichment.
- **`faq <slug> [query]`** — match question against FAQ, or list all FAQ entries.
- **`faq-dir <slug>`** — FAQ listing by category.
- **`tree <slug>`** — full hierarchy in one call (indented L3 > L2 > L1 > L0).
- **`navigate <slug> "<question>"`** — one-shot QA. Searches + fetches + synthesizes a direct answer with citations. Costs 1 LLM call.

### Analysis

`entities`, `terms`, `corrections`, `edges`, `threads`, `meta`, `resolved`

- **`entities <slug>`** — all extracted entities.
- **`terms <slug>`** — vocabulary with definitions.
- **`corrections <slug>`** — corrections the pyramid made to source material.
- **`edges <slug>`** — lateral connections between nodes.
- **`threads <slug>`** — L0→L1 grouping logic.
- **`meta <slug>`** — meta-analysis nodes.
- **`resolved <slug>`** — question/gap resolution status.

### Operations

`dadbear`, `cost`, `stale-log`, `usage`, `diff`

- **`dadbear <slug>`** — auto-update status: enabled/disabled, last check, debounce, breaker state.
- **`cost <slug> [--build <build_id>]`** — token + dollar cost, optionally filtered to a build.
- **`stale-log <slug> [--limit N]`** — staleness evaluations: which nodes, when, why.
- **`usage <slug> [--limit N]`** — most-accessed nodes.
- **`diff <slug>`** — changelog approximation (stale-log + build status).

### Annotations

`annotations`, `annotate`, `react`

- **`annotations <slug> [node_id]`** — list annotations, optionally filtered to a node.
- **`annotate <slug> <node_id> <content> [--question "..."] [--author "..."] [--type <type>]`** — add annotation. Types: `observation`, `correction`, `question`, `friction`, `idea`.
- **`react <slug> <annotation_id> up|down [--agent <name>]`** — vote on an annotation.

### Composite

`handoff`, `compare`

- **`handoff <slug>`** — full onboarding bundle (apex + FAQ + annotations + DADBEAR in parallel).
- **`compare <slug1> <slug2>`** — cross-pyramid: shared/unique terms, conflicting definitions, structural diffs.

### Question pyramids

`create-question-slug`, `question-build`, `references`, `composed`

- **`create-question-slug <name> --ref <slug1> [--ref <slug2> ...]`** — create a question pyramid referencing source slugs.
- **`question-build <slug> "<question>" [--granularity N] [--max-depth N]`** — decompose + build.
- **`references <slug>`** — what this slug references, what references it.
- **`composed <slug>`** — composed view across a question slug + all references.

### Vine conversations

`vine-build`, `vine-bunches`, `vine-eras`, `vine-decisions`, `vine-entities`, `vine-threads`, `vine-drill`, `vine-rebuild-upper`, `vine-integrity`

CLI-only (no MCP tool equivalents yet). Use for JSONL-based vine conversation building and inspection.

### Coordination

`session-register`, `sessions`

- **`session-register <slug> [--agent <name>]`** — register an agent session.
- **`sessions <slug>`** — list recent agent sessions.

### Primer

Primer and slope — onboarding summaries and structural overviews. Use for cold-start orientation.

### Reading

Reading modes — memoir, walk, thread, decisions, speaker, search views. Purpose-built views for specific reading experiences.

### Manifest

Manifest and runtime — cold-start bundles and manifest operations. For packaged pyramid delivery.

### Vocabulary

Terms, recognition, and diffs at the vocabulary level (distinct from `terms` which operates on a single pyramid).

### Recovery

Pyramid recovery status.

### Demand-gen

Demand generation — job status tracking. Used by long-running demand-driven expansions.

### Preview

Preview — dry-run content processing before committing to a full build.

Run `pyramid-cli help --category <name>` for the exact command list per category.

---

## Output

- **`--pretty`** (default) — pretty-printed JSON.
- **`--compact`** — minified JSON, suitable for piping.
- **`--verbose`** — extra diagnostics on stderr.

stdout is always JSON. stderr is for diagnostics. This makes CLI output safe to pipe into `jq`, `python -c "import json,sys"`, or similar.

---

## Response enrichments (client-side)

Some commands enrich the backend response before returning:

- **`drill`** — injects inline annotations for the drilled node, builds a breadcrumb trail from apex via parent_id walk (up to 5 levels).
- **`search`** — client-side re-ranks results by query term frequency in snippets. Returns `_hint` on 0 results suggesting `faq`.
- **`faq`** — returns `_hint` on 0 matches suggesting `search`.
- **`apex --summary`** — strips the response.
- **Error responses** — inject `_hint` with actionable suggestions ("Run `pyramid-cli slugs` to see available pyramids").

These enrichments are CLI-side, not server-side — the raw backend is thinner than what the CLI returns.

---

## Agent onboarding flow (recommended)

When an agent connects to a pyramid for the first time:

```bash
# 1. Composite onboarding bundle
pyramid-cli handoff my-pyramid

# 2. Token-efficient apex
pyramid-cli apex my-pyramid --summary

# 3. Learn the vocabulary
pyramid-cli terms my-pyramid

# 4. Search for the specific question
pyramid-cli search my-pyramid "stale engine retry logic"

# 5. Drill a candidate
pyramid-cli drill my-pyramid L0-042

# 6. Leave a finding
pyramid-cli annotate my-pyramid L0-042 \
  "Retry caps at 3 with exponential backoff" \
  --question "What is the retry strategy?" \
  --author onboarding-agent \
  --type observation
```

This pattern — handoff → apex → terms → search → drill → annotate — is the common trajectory an agent takes on first contact with a new pyramid.

---

## Common patterns

**Cold-start QA.** `navigate` is the one-shot path (1 LLM call). `search` + `drill` is the manual path (free).

**Finding a needle.** `faq` first (someone may have answered), `search --semantic` if FTS gives nothing.

**Leaving durable knowledge.** Every `annotate` with `--question` creates or extends a FAQ entry. Use this habit.

**Multi-agent coordination.** `session-register` on start, `sessions` to see who else is active.

**Cross-pyramid analysis.** `compare` for surfacing diffs, `create-question-slug + question-build + composed` for synthesized questions.

---

## Where to go next

- [`mcp-server/README.md`](../../mcp-server/README.md) — authoritative CLI reference (ships with the repo).
- [`81-mcp-server.md`](81-mcp-server.md) — same capabilities exposed as MCP tools for Claude.
- [`82-querying-pyramids.md`](82-querying-pyramids.md) — navigation patterns in depth.
- [`83-agent-sessions.md`](83-agent-sessions.md) — session registration and coordination.
- [`84-http-operator-api.md`](84-http-operator-api.md) — the raw HTTP underneath.
