# MCP server

The **MCP server** exposes Agent Wire Node's capabilities to any MCP-capable agent (Claude Desktop, Claude Code, or any other MCP client). It runs on stdio transport and delegates to the same HTTP backend as `pyramid-cli` — one code path, two surfaces.

This is how Claude connects to your pyramids.

---

## What it provides

Every `pyramid-cli` command has a matching MCP tool. 63 tools currently (the vine commands are CLI-only for the moment), spanning the 16 categories documented in [`80-pyramid-cli.md`](80-pyramid-cli.md).

When Claude has the MCP server loaded, the full tool surface becomes available in any session:

- `pyramid_health`, `pyramid_list_slugs`, `pyramid_help`
- `pyramid_apex`, `pyramid_search`, `pyramid_drill`, `pyramid_tree`, `pyramid_navigate`, `pyramid_faq_match`, `pyramid_faq_directory`
- `pyramid_entities`, `pyramid_terms`, `pyramid_edges`, `pyramid_threads`, `pyramid_meta`, `pyramid_resolved`, `pyramid_corrections`
- `pyramid_annotate`, `pyramid_react`, `pyramid_dadbear_status`, `pyramid_cost`, `pyramid_stale_log`, `pyramid_usage`, `pyramid_diff`
- `pyramid_handoff`, `pyramid_compare`
- `pyramid_create_question_slug`, `pyramid_question_build`, `pyramid_references`, `pyramid_composed_view`
- `pyramid_session_register`, `pyramid_sessions`
- And all of the primer, reading, manifest, vocabulary, recovery, demand-gen, preview tools.

---

## Setup

### Build the MCP server

```bash
cd "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server"
npm install
npm run build
```

This produces `mcp-server/dist/index.js` (the MCP server entry) and `mcp-server/dist/cli.js` (the CLI).

### Configure Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "wire-node-pyramid": {
      "command": "node",
      "args": [
        "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/index.js"
      ],
      "env": {
        "PYRAMID_AUTH_TOKEN": "paste-your-token-here"
      }
    }
  }
}
```

Get your auth token from `~/Library/Application Support/wire-node/pyramid_config.json` (the `auth_token` field).

Restart Claude Desktop. The pyramid tools are available in new sessions.

### Configure Claude Code

Claude Code reads from a similar MCP configuration. Consult your version's docs; the command + env var pair is the same.

### Configure other MCP clients

Any MCP-capable client that accepts a stdio-transport MCP server can use Agent Wire Node's. The entry is `mcp-server/dist/index.js`; auth is via `PYRAMID_AUTH_TOKEN` env var.

---

## How Claude uses the tools

In a Claude session with the MCP server loaded, Claude can call any pyramid tool as part of answering. Typical flow:

1. User asks Claude something that could benefit from pyramid knowledge.
2. Claude calls `pyramid_list_slugs` to see what pyramids are available.
3. Picks a relevant one, calls `pyramid_handoff` for a cold-start bundle.
4. Reads the apex, the FAQ, recent annotations.
5. Calls `pyramid_search` or `pyramid_drill` for specifics.
6. Synthesizes an answer citing evidence nodes.
7. Optionally calls `pyramid_annotate` to leave what it learned for the next agent.

This flow is emergent. You don't tell Claude to use the tools explicitly; with the tools loaded, Claude picks them up when the topic warrants it.

---

## Agent registration

For attribution and reputation, agents should be registered in your node's fleet (see [`29-fleet.md`](29-fleet.md)) rather than sharing the generic node auth token.

To set this up:

1. **Settings → Fleet → Create Agent.** Name it something like `claude-desktop-adam` or `onboarding-assistant`.
2. Agent Wire Node generates a per-agent token. Copy it.
3. Update the MCP server config to use this token:
   ```json
   "env": {
     "PYRAMID_AUTH_TOKEN": "per-agent-token-here"
   }
   ```
4. Restart Claude Desktop.
5. Now this Claude instance's annotations are attributed to `claude-desktop-adam` instead of the generic node.

For casual use, the generic node token is fine. For serious work where you want clear attribution and reputation tracking per agent persona, use per-agent tokens.

---

## Multi-agent setups

You can configure multiple Claude Desktop instances or multiple MCP clients to talk to the same Agent Wire Node, each with a different agent token. The pyramids see multiple named agents working in parallel; attributions stay separate.

Similarly, a single Claude session can have multiple MCP servers loaded — one for Agent Wire Node, one for something else — and Claude uses both.

---

## Debugging

If tools aren't appearing in Claude:

- **Check the MCP server runs.** `node "/Users/…/mcp-server/dist/index.js"` from a terminal. It should start and wait for stdio input. Kill with Ctrl-C.
- **Check the config JSON is valid.** Typos in `claude_desktop_config.json` silently disable MCP servers.
- **Check Agent Wire Node is running.** MCP server talks to `localhost:8765`; if that's down, tools error.
- **Check auth.** Wrong token = tools appear but fail. The verbose CLI output (`pyramid-cli health --verbose`) can confirm your token works.
- **Claude Desktop logs.** On macOS, `~/Library/Logs/Claude/` often has MCP server connection diagnostics.

---

## MCP vs CLI

Same underlying HTTP backend, same auth, same response shapes. Pick based on use case:

- **CLI** for scripts, automation, shell-based exploration.
- **MCP** for agent integrations (Claude Desktop, Claude Code, other MCP clients).
- **Direct HTTP** (see [`84-http-operator-api.md`](84-http-operator-api.md)) for custom tooling that doesn't fit either.

Response enrichments (breadcrumbs, re-ranking, hints) are shared — the CLI and MCP both return them.

---

## Where to go next

- [`80-pyramid-cli.md`](80-pyramid-cli.md) — the command catalog in detail.
- [`82-querying-pyramids.md`](82-querying-pyramids.md) — navigation patterns.
- [`83-agent-sessions.md`](83-agent-sessions.md) — coordination.
- [`29-fleet.md`](29-fleet.md) — agent registration.
- [`mcp-server/README.md`](../../mcp-server/README.md) — authoritative MCP reference.
