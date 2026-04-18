# HTTP operator API

Agent Wire Node exposes an HTTP API on `localhost:8765` for operator and agent use. The CLI and MCP server are thin wrappers over this API; anything they do, you can do via raw HTTP.

This doc covers the operator-facing routes — roughly 25 of them as of this writing, plus the broader pyramid read/write surface. Useful when writing custom tooling that doesn't fit either the CLI or the MCP pattern.

---

## Base

- **Base URL:** `http://localhost:8765` (hardcoded).
- **Auth:** bearer token on `Authorization` header.
  - Value: your node's `auth_token` from `~/Library/Application Support/wire-node/pyramid_config.json`, or a per-agent token from the fleet.
  - Header: `Authorization: Bearer <token>`.
- **Content:** JSON on request/response bodies. `Content-Type: application/json`.
- **Errors:** 4xx/5xx with `{"error": "..."}` bodies.

---

## Operator route groups

Routes are prefixed `/pyramid/`. The operator-specific ones require the node's bearer token (not a remote-distributed token); they can mutate node state (offers, market enable/disable, model loading).

### Compute market

| Route | Method | Purpose |
|---|---|---|
| `/pyramid/compute/offers` | `POST` | Create a compute offer. |
| `/pyramid/compute/offers/:model_id` | `PUT` | Upsert an offer for a model. |
| `/pyramid/compute/offers/:model_id` | `DELETE` | Withdraw an offer. |
| `/pyramid/compute/offers` | `GET` | List all your active offers. |
| `/pyramid/compute/market/surface?model_id=...` | `GET` | Market surface (pricing, availability, demand) for a model. |
| `/pyramid/compute/market/enable` | `POST` | Opt into the market as a provider. |
| `/pyramid/compute/market/disable` | `POST` | Opt out. |
| `/pyramid/compute/market/state` | `GET` | Live state (in-flight jobs, counters, queue mirrors). |
| `/pyramid/compute/policy` | `GET` | Current compute participation policy. |
| `/pyramid/compute/policy` | `PUT` | Update policy. |
| `/pyramid/compute/market-call` | `POST` | Phase 3 smoke test — one-shot market inference dispatch. |

### System observability

| Route | Method | Purpose |
|---|---|---|
| `/pyramid/system/health` | `GET` | Node status, version, node_id, auth state, tunnel state, credits snapshot. |
| `/pyramid/system/credits` | `GET` | Current balance + annual equivalent. |
| `/pyramid/system/work-stats` | `GET` | Rolling job stats. |
| `/pyramid/system/fleet-roster` | `GET` | Connected fleet peers. |
| `/pyramid/system/tunnel` | `GET` | Tunnel status and public endpoint. |
| `/pyramid/system/auth` | `GET` | Whoami: node_id, operator_id, has_api_token, session expiry. Never leaks secrets. |
| `/pyramid/system/compute/events` | `GET` | Compute chronicle stream (queryable). |
| `/pyramid/system/compute/summary` | `GET` | Aggregated compute stats. |
| `/pyramid/system/compute/timeline` | `GET` | Timeline view. |
| `/pyramid/system/compute/chronicle-dimensions` | `GET` | Filterable dimensions. |

### Local mode control

| Route | Method | Purpose |
|---|---|---|
| `/pyramid/:slug/local-mode` | `GET` | Status snapshot (node-scoped; slug is for URL symmetry). |
| `/pyramid/:slug/local-mode/enable` | `POST` | Enable local Ollama mode. |
| `/pyramid/:slug/local-mode/disable` | `POST` | Disable. |
| `/pyramid/:slug/local-mode/switch-model` | `POST` | Switch local model. |
| `/pyramid/:slug/providers` | `GET` | List providers + health. |

### Fleet dispatch (WS-FLEET)

| Route | Method | Purpose |
|---|---|---|
| `/v1/compute/fleet-dispatch` | `POST` | Announce pending job to fleet. |
| `/v1/fleet/announce` | `POST` | Peer heartbeat / discovery. |
| `/v1/fleet/result` | `POST` | Peer reports completed job result. |

### Compute requester (Phase 3)

| Route | Method | Purpose |
|---|---|---|
| `/v1/compute/job-dispatch` | `POST` | Requester-side push-delivery entry point. |
| `/v1/compute/job-result` | `POST` | Requester-side push-delivery result handler. |

### Basic health + tunnel

| Route | Method | Purpose |
|---|---|---|
| `/health` | `GET` | Basic "is the server up" check. Does not require auth. |
| `/stats` | `GET` | Server stats (uptime, cache size, etc.). |
| `/tunnel-status` | `GET` | Tunnel connection state. |
| `/hooks/openrouter` | `POST` | OpenRouter webhook callback. |
| `/auth/callback` | `POST` | Supabase magic-link callback. |
| `/auth/complete` | `POST` | OTP verification. |
| `/documents/:id` | `GET` | Retrieve cached documents (JWT-verified for remote-safe access). |

---

## Pyramid read/write surface

Beyond the operator routes, the same server exposes the full pyramid API — the thing `pyramid-cli` and the MCP server call. These routes are under `/pyramid/:slug/...` and cover:

- **Read:** `apex`, `search`, `drill`, `tree`, `entities`, `terms`, `faq`, and all the 64 CLI command equivalents.
- **Write:** `annotations`, `react`, `session`, `build`, `dadbear`, `publish`.

The exact route shapes follow the CLI command shapes. `pyramid-cli search my-pyramid "query"` maps to `GET /pyramid/my-pyramid/search?q=query`. The MCP server's tools map similarly.

Rather than document every pyramid route here, use:

- **`pyramid-cli help`** for the full command catalog (which maps 1:1 to routes).
- **`mcp-server/src/index.ts`** in the repo for the authoritative tool-to-route mapping.

---

## Curl examples

### Check status

```bash
TOKEN=$(python3 -c "import json; print(json.load(open('$HOME/Library/Application Support/wire-node/pyramid_config.json'))['auth_token'])")

curl -s -H "Authorization: Bearer $TOKEN" \
  http://localhost:8765/pyramid/system/health | jq
```

### List pyramids

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  http://localhost:8765/pyramid/slugs | jq
```

### Drill a node

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:8765/pyramid/my-pyramid/drill/L1-003" | jq
```

### Create a compute offer

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -X POST "http://localhost:8765/pyramid/compute/offers" \
  -d '{
    "model": "gemma3:27b",
    "rate_per_1k_tokens": 3,
    "capacity": 2
  }' | jq
```

### Enable the market

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  -X POST "http://localhost:8765/pyramid/compute/market/enable" | jq
```

### Annotate a node

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -X POST "http://localhost:8765/pyramid/my-pyramid/annotations" \
  -d '{
    "node_id": "L0-012",
    "content": "Retry caps at 3.",
    "question_context": "How many retries?",
    "author": "auditor-1",
    "type": "observation"
  }' | jq
```

---

## Tunnel considerations

Your Agent Wire Node's HTTP server is reachable via:

- **localhost:8765** — on your machine, direct.
- **Your tunnel URL** — the public Cloudflare Tunnel endpoint. Other nodes and the coordinator reach you through this.

Operator routes (bearer-auth required) work over both surfaces; the auth token is what gates access, not the network path. Public routes (health, document retrieval with JWT) are routable from anywhere.

Don't distribute your bearer token — anyone with it can mutate node state.

---

## Response shapes

Responses are JSON objects. Common shapes:

- **Success:** the data directly (e.g. `{"node": {...}}` for a drill, or an array for list operations).
- **Error:** `{"error": "description"}` with a 4xx or 5xx status.
- **Hint-enriched errors:** `{"error": "...", "_hint": "try X"}` — the CLI adds these client-side, but some backend errors do include them.

No standardized envelope — each endpoint returns whatever shape is natural. `pyramid_help` includes per-command response schemas if you need them programmatically.

---

## When to use raw HTTP

Pick the raw HTTP surface when:

- You're writing a custom tool that doesn't fit the CLI or MCP patterns.
- You need to hit operator-specific routes that aren't exposed via MCP (most of the compute market management, tunnel control, local-mode switching — these are operator-only).
- You want fine-grained control over request headers, body, or timeouts.

Pick the CLI or MCP when:

- The CLI command covers what you need (almost always for exploration).
- You want the client-side enrichments (breadcrumbs, re-ranking, hints).
- You want easy agent integration (MCP).

---

## Where to go next

- [`80-pyramid-cli.md`](80-pyramid-cli.md) — the CLI over the same API.
- [`81-mcp-server.md`](81-mcp-server.md) — the MCP server over the same API.
- [`34-settings.md`](34-settings.md) → Agent Wire Node Settings — where your auth token lives.
- [`mcp-server/README.md`](../../mcp-server/README.md) — authoritative route-to-tool mapping.
