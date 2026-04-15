# Node Identity: Handle Paths

**Date:** 2026-04-15
**Problem:** The identity model conflates agents with nodes. `wire_nodes.agent_id` is a structural FK that ties one machine to one identity. Two machines under the same operator share one agent → one node → one overwrites the other. Fleet routing is impossible.
**Root cause:** Nodes don't have their own identity. They borrow the agent's. The agent was designed as the primary actor, but it's actually a "hand" — an identity the operator deploys. The node (machine) is the physical thing.
**Fix:** Nodes get handle paths. `@hello/BEHEM` is a first-class identity, not a UUID. The three axes — operator, node, agent — are fully decoupled.

---

## The 100-Year Model

Three independent concepts:

| Concept | What it is | Identity | Example |
|---|---|---|---|
| **Operator** | The human (or org) | `@hello` | Adam |
| **Node** | A physical machine | `@hello/BEHEM` | The 5090 downstairs |
| **Agent** | An identity on the Wire | `@hello/research-bot` | A reputation + handle + credits |

Relationships:
- Operator → many Nodes (home lab, office, cloud VPS)
- Operator → many Agents (personal, business, anonymous)
- Node has one active Agent at a time (runtime binding, changeable)
- Agent can be active on multiple Nodes simultaneously (distributed identity)

```
@hello                    — operator
├── /BEHEM                — node (5090, 24GB VRAM)
│   └── agent: @hello/main
├── /macbook              — node (M-series, 128GB unified)
│   └── agent: @hello/main
├── /cloud-1              — node (VPS, no GPU, relay only)
│   └── agent: @hello/relay-service
├── @hello/main           — agent (primary identity, reputation, credits)
├── @hello/relay-service  — agent (anonymous relay identity)
└── @hello/research       — agent (separate research identity)
```

## Handle Path Syntax

Node handle paths follow the existing Wire handle convention:

```
@{operator_handle}/{node_handle}
```

- `@hello/BEHEM` — full path, globally unique
- `BEHEM` — local part, unique within operator's namespace
- The `/` separates operator from node (same as filesystem intuition)

Extended paths for sub-resources:
```
@hello/BEHEM:ollama-catchall              — a serving rule on BEHEM
@hello/macbook:opt-025:extract_l0:d0:c3   — a chronicle job path on macbook
fleet:@hello/BEHEM:ollama-catchall:17131   — a fleet dispatch to BEHEM
```

The `:` separates the node handle from sub-resource paths. The `@operator/node` part is always the prefix.

## Schema Changes

### wire_nodes: Add operator_id, node_handle, decouple from agent

**Phase 1a migration** (zero breaking changes -- agent_id stays untouched):

```sql
-- New columns (agent_id stays as-is for now)
ALTER TABLE wire_nodes ADD COLUMN operator_id UUID REFERENCES wire_operators(id);
ALTER TABLE wire_nodes ADD COLUMN node_handle TEXT;
ALTER TABLE wire_nodes ADD COLUMN node_token_hash TEXT;  -- bcrypt of reconnection token

-- agent_id stays for now. Phase 1b renames it after all 20+ consumers are updated.

-- Backfill operator_id from the agent→operator chain
UPDATE wire_nodes SET operator_id = (
    SELECT a.operator_id FROM wire_agents a WHERE a.id = wire_nodes.agent_id
) WHERE operator_id IS NULL;

-- Backfill node_handle from existing name with collision avoidance
DO $$
DECLARE
  r RECORD;
  candidate TEXT;
  suffix INTEGER;
BEGIN
  FOR r IN SELECT id, operator_id, name FROM wire_nodes WHERE node_handle IS NULL LOOP
    candidate := COALESCE(NULLIF(TRIM(r.name), ''), SUBSTRING(r.id::text FROM 1 FOR 8));
    candidate := LOWER(REGEXP_REPLACE(candidate, '[^a-z0-9-]', '-', 'g'));
    candidate := TRIM(BOTH '-' FROM candidate);
    IF LENGTH(candidate) = 0 THEN candidate := 'node'; END IF;
    IF LENGTH(candidate) > 20 THEN candidate := SUBSTRING(candidate FROM 1 FOR 20); END IF;
    suffix := 0;
    WHILE EXISTS (SELECT 1 FROM wire_nodes WHERE operator_id = r.operator_id AND node_handle = candidate || CASE WHEN suffix = 0 THEN '' ELSE '-' || suffix::text END) LOOP
      suffix := suffix + 1;
    END LOOP;
    UPDATE wire_nodes SET node_handle = candidate || CASE WHEN suffix = 0 THEN '' ELSE '-' || suffix::text END WHERE id = r.id;
  END LOOP;
END $$;

-- Make NOT NULL after backfill
ALTER TABLE wire_nodes ALTER COLUMN operator_id SET NOT NULL;
ALTER TABLE wire_nodes ALTER COLUMN node_handle SET NOT NULL;

-- Unique constraint: one handle per operator
ALTER TABLE wire_nodes ADD CONSTRAINT wire_nodes_operator_handle_unique
    UNIQUE(operator_id, node_handle);

-- Index for fleet roster query (direct, no agent join)
CREATE INDEX idx_wire_nodes_operator ON wire_nodes(operator_id, status)
    WHERE status = 'online';
```

### Registration: by (operator_id, node_handle)

The `register-with-session` handler changes:

```
Before: operator → find agent → find node by agent_id → upsert one node
After:  operator → node_handle from request → find node by (operator_id, node_handle)
        → if found AND token matches: reconnect (update tunnel, status)
        → if found AND token doesn't match: error "handle taken"
        → if not found: create new node, set agent_id to operator's shared desktop agent
```

### Fleet heartbeat: direct operator query

```sql
-- Before (3-table join):
SELECT n.* FROM wire_nodes n
  JOIN wire_agents a ON a.id = n.agent_id
  WHERE a.operator_id = $1 AND n.status = 'online'

-- After (direct):
SELECT n.id, n.node_handle, n.tunnel_url FROM wire_nodes n
  WHERE n.operator_id = $1 AND n.status = 'online' AND n.id != $2
```

The fleet roster returns BOTH `node_id` (UUID) AND `handle_path` during the transition. Old nodes expect `node_id`; new nodes use `handle_path` when present, fall back to `node_id`. This prevents silent fleet peer disappearance on rolling updates. The frontend shows `@hello/BEHEM`.

## Node-Side Changes

### First launch: choose a name

On first launch (no `node_identity.json` in data dir):
1. Auto-generate from hostname using `gethostname` crate (POSIX system call, NOT env vars -- env vars are often empty in macOS GUI contexts)
2. Sanitize, lowercase, truncate to 20 chars
3. If hostname is generic ("localhost"), generate: `node-{4-char-random}`
4. Display to operator in the existing onboarding flow (see below)
5. Operator can edit before completing setup

**Migration for existing installs:** On first boot after upgrade, if `node_identity.json` doesn't exist, derive `node_handle` from `onboarding.json`'s `node_name` field (if present), falling back to hostname via `gethostname`.

**Handle 409 on first registration after upgrade:** Existing installs that derive `node_handle` from a generic name like "Wire Node" will collide with other machines under the same operator (the SQL backfill handles server-side collisions, but the Rust-side migration does not). If registration returns 409 "handle taken", automatically append a random 4-char suffix (e.g., `wire-node-a7f3`) to the handle and retry. Maximum 3 retries. If all 3 fail, surface the error to the user. Update `node_identity.json` with the successful handle before proceeding.

Handle generation function (uses `gethostname` crate, not env vars):

```rust
fn generate_default_handle() -> String {
    let raw = gethostname::gethostname()
        .to_string_lossy()
        .to_lowercase();
    let sanitized: String = raw.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() || trimmed == "localhost" {
        format!("node-{}", &uuid::Uuid::new_v4().to_string()[..4])
    } else if trimmed.len() > 20 {
        trimmed[..20].to_string()
    } else {
        trimmed.to_string()
    }
}
```

Note: the UUID here is for the random suffix only (4 chars), not as an identity. The handle itself is semantic.

**Token generation:** Use `rand` crate to generate 32 random bytes, hex-encode, prefix with `nt_`. The `nt_` prefix makes tokens identifiable if they leak (same pattern as API tokens with `sk_` prefixes).

Stored in `{data_dir}/node_identity.json`:
```json
{
    "node_handle": "BEHEM",
    "node_token": "nt_a7f3b2c1d4e5..."
}
```

The `node_token` is a random secret generated once. Sent in registration requests. The server stores `bcrypt(node_token)` as `node_token_hash`. This proves "I am the machine that registered as BEHEM" on reconnection.

### Registration request: sends node_handle + node_token

```rust
// In auth.rs, register_with_session():
POST /api/v1/node/register-with-session
{
    "session_token": "...",       // Supabase auth token
    "node_handle": "BEHEM",       // chosen name
    "node_token": "nt_a7f3b2c1...", // reconnection proof
    "name": "BEHEM",              // display name (can differ from handle)
    "app_version": "0.3.0"
}
```

### LlmConfig + fleet_roster: handle paths instead of UUIDs

Everywhere the codebase references a node_id (fleet roster, fleet dispatch, chronicle events), use the handle path `@{operator_handle}/{node_handle}` instead.

`FleetPeer`:
```rust
pub struct FleetPeer {
    pub node_id: String,              // UUID — kept for backward compatibility during transition
    pub handle_path: Option<String>,  // "@hello/BEHEM" — new nodes prefer this when present
    pub tunnel_url: String,
    pub serving_rules: Vec<String>,
    pub total_queue_depth: usize,
    pub last_seen: DateTime<Utc>,
}
```

`FleetAnnouncement` -- **ADD `node_handle` and `operator_handle` as NEW fields. Do NOT remove any existing fields.** The existing struct has `node_id`, `name`, `operator_id`, `queue_depths` that are used by the roster update and fleet announce handler. Same transition pattern as HeartbeatFleetEntry: both old and new fields present during rollout.
```rust
pub struct FleetAnnouncement {
    pub node_id: String,                         // kept — existing field, used by roster update
    pub name: Option<String>,                    // kept — existing field
    pub node_handle: Option<String>,             // NEW — "BEHEM" (local part)
    pub operator_handle: Option<String>,         // NEW — "hello"
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,
    pub serving_rules: Vec<String>,
    pub queue_depths: HashMap<String, usize>,    // kept — existing field
    pub total_queue_depth: usize,
    pub operator_id: String,                     // kept — existing field
}
```

### Chronicle job paths: include node handle

```
@hello/BEHEM:opt-025:stale_check:d1:node-abc      — BEHEM processed this
@hello/macbook:opt-025:extract_l0:d0:chunk-3       — macbook processed this
fleet:@hello/BEHEM:ollama-catchall:1713145623       — dispatched TO BEHEM
fleet-recv:@hello/macbook:ollama-catchall:1713145   — received FROM macbook
```

The `generate_job_path` function uses `node_handle` from the local identity, not a UUID.

### LlmResponse fleet provenance: handle paths

```rust
pub fleet_peer_id: Option<String>,     // "@hello/BEHEM" — not a UUID
pub fleet_peer_model: Option<String>,
```

## Wire-Side Changes (GoodNewsEveryone)

### Agent model: one shared agent, not one per machine

The current model creates one desktop agent per operator. All machines under one operator share the same agent. The new node row gets `agent_id` pointing to the shared desktop agent. The `node_token` is node-scoped (proves "I am this machine"), not agent-scoped. This is explicit: adding a new machine does NOT create a new agent.

### Registration handler: (operator_id, node_handle) lookup

**Registration and heartbeat responses must include `operator_handle`.** The node needs the operator's handle to construct full paths like `@hello/BEHEM`. Resolution chain: `operator_handle = claimed_wire_handle ?? login_email`. Operators must claim a Wire handle to get a short handle; until they do, the login email IS the handle. So `@hello/BEHEM` if handle claimed, `@hello@callmeplayful.com/BEHEM` if not. The `operator_handle` is NEVER null — it always resolves to at least the email. Return `operator_handle: String` (not nullable) in both the registration and heartbeat responses. The Rust `SessionRegistrationResponse` struct must add `pub operator_handle: Option<String>` (Option for serde compat with old servers). Store in `AuthState`. Also returned in heartbeat response for freshness (handle may be claimed between heartbeats).

```typescript
// In register-with-session/route.ts:

// 1. Verify Supabase token → get email → get operator
// 2. Read node_handle and node_token from request
// 3. Look up existing node:
const { data: existingNode } = await adminClient
    .from('wire_nodes')
    .select('id, node_token_hash')
    .eq('operator_id', operatorId)
    .eq('node_handle', nodeHandle)
    .maybeSingle();

if (existingNode) {
    // Reconnection — verify token
    if (!await bcrypt.compare(nodeToken, existingNode.node_token_hash)) {
        return Response.json({ error: 'Handle taken by another machine' }, { status: 409 });
    }
    // Update existing node (tunnel, status, version)
    nodeId = existingNode.id;
} else {
    // New machine — reuse operator's shared desktop agent, create new node
    // All machines share one agent per operator. agent_id points to the shared desktop agent.
    // The node_token is node-scoped, not agent-scoped.
    const tokenHash = await bcrypt.hash(nodeToken, 10);
    // NOTE: wire_agents.agent_type CHECK constraint only allows 'autonomous', 'human', 'platform'.
    // There is no 'desktop' type. Use the existing lookup pattern from line 181 of
    // register-with-session/route.ts: find the desktop agent via api_clients.name LIKE.
    const { data: sharedAgent } = await adminClient
        .from('wire_agents')
        .select('id, api_clients!inner(id, name)')
        .eq('operator_id', operatorId)
        .like('api_clients.name', 'Wire Node Desktop%')
        .limit(1)
        .maybeSingle();
    // ... create node with operator_id, node_handle, node_token_hash, agent_id = sharedAgent.id
}
```

### Token recovery: reclaim-handle endpoint

If the operator reinstalls and loses `node_identity.json`, they get a 409 "handle taken" on registration. This endpoint lets them reclaim the handle:

```
POST /api/v1/node/reclaim-handle
Body: { supabase_access_token, node_handle, new_node_token }
```

Flow:
1. Verify `supabase_access_token` → resolve operator
2. Look up `wire_nodes` where `(operator_id, node_handle)` matches
3. If found and operator owns it: overwrite `node_token_hash` with `bcrypt(new_node_token)`
4. Return success. The node can now register normally with the new token.

This is operator-scoped: only the operator who owns the handle can reclaim it. No admin intervention needed.

### Heartbeat: fleet roster with handle paths

```typescript
// In heartbeat/route.ts:
const { data: fleetNodes } = await adminClient
    .from('wire_nodes')
    .select('id, node_handle, tunnel_url, operator_id')
    .eq('operator_id', thisOperatorId)
    .eq('status', 'online')
    .not('id', 'eq', nodeId)
    .not('tunnel_url', 'is', null);

// Resolve operator handle for full path (query wire_handles by operator_id, same as Settings page)
// Return operator_handle in the heartbeat response alongside the fleet roster.
// This ensures the node always has the latest operator handle even if it was registered after initial setup.
const operatorHandle = await getOperatorHandle(thisOperatorId);

// Return BOTH node_id and handle_path for rolling-update compatibility.
// Old nodes expect node_id. New nodes use handle_path when present, fall back to node_id.
fleetRoster = (fleetNodes ?? []).map(n => ({
    node_id: n.id,                                       // kept for old nodes
    handle_path: `@${operatorHandle}/${n.node_handle}`,  // new nodes prefer this
    tunnel_url: n.tunnel_url,
}));
```

The Rust `HeartbeatFleetEntry` struct adds an optional field for backward compatibility:

```rust
pub struct HeartbeatFleetEntry {
    pub node_id: String,                    // always present (UUID)
    pub handle_path: Option<String>,        // present when server supports it
    pub tunnel_url: String,
    // ... other fields
}
```

The heartbeat response (top level, not per-entry) must also return `operator_handle: Option<String>` so the node can update its cached operator handle. The Rust heartbeat response struct adds:
```rust
pub operator_handle: Option<String>,  // updated on every heartbeat
```

New nodes use `handle_path` when `Some`, fall back to `node_id` when `None`. This prevents silent fleet peer disappearance during rolling updates where one node is upgraded and the other is not.

### Fleet JWT: include node_handle

```typescript
// In fleet-jwt.ts:
new SignJWT({ op: operatorId, nid: nodeId, nh: nodeHandle })
    .setAudience('fleet')
    // ...
```

The receiving node validates operator AND can display "request from @hello/macbook" in logs.

## Frontend Changes

### Fleet tab → Compute sub-tab

The Fleet sidebar section gets a "Compute" sub-tab showing all nodes under this operator:

```
┌─ Fleet Overview ─┬─ Compute ─┬─ Coordination ─┬─ Tasks ─┐
│                                                           │
│  Your Nodes                           2 online            │
│                                                           │
│  ┌──────────────────────────────────────────────┐        │
│  │  @hello/BEHEM                     ● online   │        │
│  │  5090 · deepseek-r1:32b loaded               │        │
│  │  Queue: 0 · Serving: ollama-catchall         │        │
│  │  Tunnel: active                              │        │
│  └──────────────────────────────────────────────┘        │
│                                                           │
│  ┌──────────────────────────────────────────────┐        │
│  │  @hello/macbook                   ● online   │        │
│  │  M-series · gemma4:26b loaded                │        │
│  │  Queue: 0 · Serving: ollama-catchall         │        │
│  │  Tunnel: active                   ← you      │        │
│  └──────────────────────────────────────────────┘        │
│                                                           │
└───────────────────────────────────────────────────────────┘
```

This moves fleet node management from Market → Market sub-tab to Fleet → Compute sub-tab (where it belongs). The Market tab focuses on the exchange/queue.

### Settings: Node Identity

Settings page gets a "Node Identity" section:
- Node handle (read-only after initial setup, editable via rename flow)
- Active agent identity (dropdown of operator's agents — future)
- Node token (regenerate if compromised)

### Onboarding: Update Existing "Node Name" Field

The `OnboardingWizard.tsx` already has a "Node Name" field. Do NOT create a new onboarding step. Instead, update the existing field:

1. **Show full handle path preview** below the input: `@{operator_handle}/{node_handle}` (updates live as user types)
2. **Add validation:** lowercase only, no spaces, alphanumeric + hyphens only, max 20 chars
3. **Save to `node_identity.json`** alongside `onboarding.json` when the step completes

```
┌──────────────────────────────────────┐
│                                      │
│  Node Name                           │
│                                      │
│  ┌──────────────────────────┐       │
│  │ BEHEM                    │       │
│  └──────────────────────────┘       │
│  (auto-detected from hostname)       │
│                                      │
│  Your node's handle path:            │
│  @hello/BEHEM                        │
│                                      │
│  lowercase, alphanumeric + hyphens   │
│                                      │
└──────────────────────────────────────┘
```

## Implementation Steps

### Phase 1a: Schema + Registration (unblocks fleet testing, zero breaking changes)

0. **Dependencies:**
   - Wire (GoodNewsEveryone): `npm install bcryptjs @types/bcryptjs`
   - Node (agent-wire-node): Add `gethostname = "0.2"` and `rand = "0.8"` to `Cargo.toml`
1. **Migration:** Add `operator_id`, `node_handle`, `node_token_hash` to `wire_nodes`. Backfill with collision-avoidance PL/pgSQL. Add UNIQUE constraint. Add index. **Do NOT rename `agent_id`** -- it stays untouched.
2. **Node-side:** Generate `node_identity.json` on first launch. Handle derived from `gethostname` system call (not env vars). Token: 32 random bytes via `rand` crate, hex-encoded, prefixed with `nt_`. Send in registration.
   - **`NodeIdentity` must be loaded at app startup, before any registration attempt.** The `register_with_session` function changes to accept `node_handle` and `node_token`, and there are **7 call sites in main.rs** that all need updating:
     - Line 77 (initial registration on fresh start)
     - Line 148 (re-registration after token refresh)
     - Line 219 (re-registration after session recovery)
     - Line 498 (registration in background auth loop)
     - Line 531 (registration after token rotation)
     - Line 11942 (registration in fleet reconnect handler)
     - Line 12102 (registration in heartbeat failure recovery)
   - Load/generate `NodeIdentity` in the early startup sequence (before auth), store on `AppState`, pass to all call sites.
3. **Registration handler:** Change lookup from `agent_id` to `(operator_id, node_handle)`. Verify token on reconnection. New machines get `agent_id` pointing to the operator's shared desktop agent (one agent per operator, not per machine).
   - **CRITICAL -- Stop revoking old secrets on registration.** The current handler (lines 227-238 of `register-with-session/route.ts`) revokes ALL previous `api_client_secrets` when creating a new secret. Under the shared-agent model, machine B's registration invalidates machine A's API token, causing 401 on next heartbeat. The `api_client_secrets` insert (lines 212-220) must be **additive** -- each machine gets its own secret row. Remove the revocation block at lines 227-238 entirely. Orphaned secrets are cleaned up on a separate schedule (secrets with no heartbeat activity in 30+ days).
4. **Heartbeat fleet roster:** Query by `operator_id` directly. Return BOTH `node_id` (UUID) AND `handle_path` for rolling-update compatibility. Old nodes use `node_id`, new nodes prefer `handle_path`.
5. **Fleet dispatch:** Use `handle_path` in FleetPeer, FleetAnnouncement, FleetDispatchRequest. Chronicle job paths use `@operator/node` prefix.
6. **Token recovery endpoint:** `POST /api/v1/node/reclaim-handle` -- verifies operator ownership via Supabase token, overwrites `node_token_hash`. Handles reinstall/lost `node_identity.json` scenario.
7. **Update `settle_document_serve` RPC.** The current function (in `20260315990000_storage_network_rpcs.sql`) resolves `operator_id` via a 3-table join: `wire_nodes.agent_id -> wire_agents.operator_id`. Once `wire_nodes` has `operator_id` directly, replace with `SELECT n.operator_id FROM wire_nodes n WHERE n.id = p_hosting_node_id`. This validates the new column under real financial load. Include as a `CREATE OR REPLACE FUNCTION` in the Phase 1a migration.

### Phase 1b: Rename agent_id (future, after all consumers updated)

Rename `agent_id → active_agent_id` across all 20+ files that reference the column. This is a separate migration and code sweep. Not part of the initial build. See "Files Modified -- Phase 1b" table below for the full list.

### Phase 2: Frontend (Compute tab + onboarding)

8. **Fleet → Compute sub-tab:** Node cards with handle paths, models, queue depths, status.
9. **Onboarding:** Update existing "Node Name" field in OnboardingWizard.tsx with handle path preview and validation. No new step.
10. **Settings:** Node identity section.
11. **Market tab:** Remove fleet peer cards (moved to Fleet → Compute). Market tab focuses on queue + market features.

### Phase 3: Full decoupling (agent as runtime binding)

12. **Active agent selector:** Settings UI to choose which agent identity is active on this node.
13. **Multi-agent support:** Different nodes can run different agent identities. Market offers tagged by (agent, node).
14. **Agent portability:** Move an agent from one node to another (change `agent_id`, renamed to `active_agent_id` in Phase 1b).

## What NOT to Build Yet

- Agent portability (Phase 3)
- Multi-agent per node (Phase 3)
- Node handle renaming flow (handle is permanent for now)
- Handle payment/reservation (node handles are free — not a scarce namespace like @handles)
- Node handle in wire_handles table (unnecessary complexity — UNIQUE constraint on wire_nodes is sufficient)

## Critical Rules

- **No UUIDs in any user-facing or LLM-facing context.** Handle paths only. The internal UUID primary key exists for DB efficiency but never surfaces.
- **Operator owns nodes directly.** `wire_nodes.operator_id` is the ownership FK. Not through agents.
- **Handle is permanent identity.** Like a domain name. Reconnection uses the token, not the handle. If you lose the token, the operator can reclaim via `POST /api/v1/node/reclaim-handle` (authenticates via Supabase token, overwrites the stored hash).
- **One handle per machine.** A physical machine doesn't get multiple handles. If you want a different name, rename (future Phase 3).
- **Agent is runtime, not structural.** `agent_id` (renamed to `active_agent_id` in Phase 1b) can change. It's which identity this machine presents to the network, not who owns the machine.

## Files Modified

### Phase 1a files (zero breaking changes to existing code)

| File | Change |
|---|---|
| **GoodNewsEveryone** | |
| `supabase/migrations/20260415XXXXXX_node_identity.sql` | Add columns, backfill with collision avoidance, constraints, index. NO agent_id rename. Also includes `CREATE OR REPLACE FUNCTION settle_document_serve(...)` using `n.operator_id` directly instead of the 3-table join through `agent_id -> wire_agents.operator_id` -- validates the new column under real financial load. |
| `src/app/api/v1/node/register-with-session/route.ts` | (operator_id, node_handle) lookup, token verification, shared desktop agent, stop revoking old secrets (additive insert only) |
| `src/app/api/v1/node/heartbeat/route.ts` | Fleet roster by operator_id, return BOTH node_id and handle_path, return operator_handle in response |
| `src/app/api/v1/node/reclaim-handle/route.ts` | New: token recovery endpoint for reinstall/lost identity scenario |
| `src/lib/server/fleet-jwt.ts` | Include node_handle in JWT claims |
| `package.json` | Add `bcryptjs` + `@types/bcryptjs` |
| **agent-wire-node** | |
| `src-tauri/Cargo.toml` | Add `gethostname = "0.2"`, `rand = "0.8"` |
| `src-tauri/src/auth.rs` | Generate/load node_identity.json (gethostname + rand), send handle+token in registration, add `operator_handle: Option<String>` to `SessionRegistrationResponse`, 409 retry with suffix |
| `src-tauri/src/fleet.rs` | FleetPeer.handle_path, FleetAnnouncement.node_handle, HeartbeatFleetEntry with optional handle_path |
| `src-tauri/src/pyramid/llm.rs` | Fleet dispatch uses handle_path |
| `src-tauri/src/server.rs` | Fleet endpoints use handle_path |
| `src-tauri/src/main.rs` | Heartbeat parsing for handle_path roster, node identity init |
| `src-tauri/src/lib.rs` | NodeIdentity struct on AppState |

### Phase 2 files (frontend: Compute tab + onboarding)

| File | Change |
|---|---|
| **agent-wire-node** | |
| `src/components/modes/FleetMode.tsx` | Add Compute sub-tab |
| `src/components/FleetComputeTab.tsx` | New: node cards with handle paths |
| `src/components/MarketDashboard.tsx` | Remove fleet peer section (moved to Fleet) |
| `src/components/OnboardingWizard.tsx` | Update existing Node Name field: handle path preview, validation, save to node_identity.json |

### Phase 1b files (future: `agent_id` → `active_agent_id` rename)

All files that reference `wire_nodes.agent_id`. This rename happens AFTER all consumers are updated.

| File | Change |
|---|---|
| **GoodNewsEveryone** | |
| `src/app/api/v1/node/register-with-session/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/heartbeat/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/fleet-jwt/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/status/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/contribution/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/serve/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/documents/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/settle/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/credits/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/market/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/queue/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/models/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/config/route.ts` | agent_id → active_agent_id |
| `src/app/api/v1/node/pyramid/route.ts` | agent_id → active_agent_id |
| (14 API route handlers under `src/app/api/v1/node/`) | agent_id → active_agent_id |
| `src/lib/server/wire-merge.ts` | agent_id → active_agent_id |
| `src/lib/server/operator-queries.ts` | agent_id → active_agent_id |
| `supabase/migrations/` | New migration: `CREATE OR REPLACE` settle_document_serve function, rename column |
| Index `idx_wire_nodes_agent` | Recreate with new column name |

## Verification

1. Fresh install on machine A → generates handle from hostname → registers as `@hello/machineA`
2. Fresh install on machine B → generates different handle → registers as `@hello/machineB`
3. Both online → heartbeat returns each other in fleet roster with handle paths
4. Fleet dispatch references `@hello/machineB` not a UUID
5. Machine A restarts → reconnects as `@hello/machineA` using stored token
6. Machine C tries to register as "machineA" → 409 "handle taken"
7. No UUIDs in: fleet roster, chronicle events, fleet dispatch logs, frontend UI, LlmResponse fields
8. Fleet → Compute tab shows both nodes with handle paths and status
