# Handoff: Fleet Routing — Overnight Build

**Date:** 2026-04-14
**Author:** Design session with Adam. Sources: wire-compute-market-build-plan.md (Section I fleet architecture + Phase 2), handoff-2026-04-13-wire-markets.md (Phase 1 infrastructure already built), market-seed-contributions.md, codebase audit of existing tunnel/JWT/heartbeat/queue infrastructure.
**Scope:** Fleet-internal routing only. Same-operator nodes dispatch LLM calls to each other directly. No exchange, no credits, no relay. This is the focused Phase 2 subset.

---

## What This Is

Adam has a 5090 GPU downstairs on one node and a laptop upstairs on another node. Both are under the same Wire operator account (same email). The laptop should be able to dispatch LLM calls to the 5090 when it has the right model loaded and has capacity.

Fleet routing characteristics:
- **No credits, no settlement.** Same operator's hardware. Cost = electricity.
- **No Wire proxy.** Direct peer-to-peer via Cloudflare tunnels.
- **No exchange involvement.** Bypasses the order book entirely.
- **Fleet authentication via Wire-signed JWT.** The Wire vouches for identity (like a CA) but never sees fleet traffic.
- **Fleet discovery via heartbeat response + direct peer announcement.**

## Documents (read in this order)

1. **`docs/plans/wire-compute-market-build-plan.md`** — Lines 86-112 for fleet architecture. Lines 1296-1420 for fleet data structures and endpoints.
2. **`docs/plans/handoff-2026-04-13-wire-markets.md`** — What Phase 1 built (compute queue, GPU loop, transparent LLM routing). Fleet dispatch feeds into this same queue.
3. **`docs/plans/market-seed-contributions.md`** — No fleet-specific seeds needed. Fleet routing is free (no pricing contributions).

## What Already Exists (Phase 1 Infrastructure)

Everything below was built in the Phase 1 overnight session and is live:

**Compute Queue (`src-tauri/src/compute_queue.rs`):**
- `ComputeQueueHandle` — cloneable Arc-wrapped handle with Mutex queue + Notify signal
- `ComputeQueueManager` — per-model FIFO queues, round-robin dequeue
- `QueueEntry` — carries full LlmConfig, prompts, temperature, max_tokens, response_format, StepContext, options
- `enqueue_local(model_id, entry)` — push to model queue
- `dequeue_next()` — round-robin pop
- `queue_depth(model_id)` / `total_depth()` — depth queries

**Transparent LLM Integration (`src-tauri/src/pyramid/llm.rs`):**
- `LlmConfig.compute_queue: Option<ComputeQueueHandle>` — when Some, all LLM calls auto-enqueue
- Interception at `call_model_unified_with_audit_and_ctx` (~line 626): if compute_queue is Some and skip_concurrency_gate is false, enqueue + await oneshot result
- GPU config clone has `compute_queue: None` (prevents re-enqueue) and `skip_concurrency_gate: true`

**GPU Processing Loop (`src-tauri/src/main.rs` ~line 11267):**
- Background task that waits on `queue_handle.notify`, then drains round-robin
- Calls `call_model_unified_with_audit_and_ctx` with the entry's config (compute_queue: None)
- Panic guard: catches panics so the loop survives

**Tunnel Infrastructure (`src-tauri/src/tunnel.rs`):**
- `TunnelState` — persisted to `tunnel.json`, has `tunnel_url: Option<String>`
- Cloudflare tunnel provisioning via `POST {api_base_url}/api/v1/node/tunnel`
- Tunnel URL is HTTPS, validated for SSRF in heartbeat handler
- Tunnel URL is sent on every heartbeat and stored on `wire_nodes.tunnel_url`

**JWT Infrastructure:**
- Wire uses Ed25519 (EdDSA) keypair: `WIRE_DOCUMENT_JWT_PRIVATE_KEY` / `WIRE_DOCUMENT_JWT_PUBLIC_KEY` env vars
- Wire signs JWTs via `jose` library (`GoodNewsEveryone/src/lib/server/document-jwt.ts`)
- Node receives public key at registration (`auth.rs:317 — SessionRegistrationResponse.jwt_public_key`)
- Node stores public key in `server.rs:24 — ServerState.jwt_public_key: Arc<RwLock<String>>`
- Node verifies JWTs via `jsonwebtoken` crate (`server.rs:1239 — verify_jwt()`, `server.rs:1352 — verify_pyramid_query_jwt()`)
- Existing JWT audience types: `"pyramid-query"` (for pyramid queries), none/default (for document tokens)

**Heartbeat (`src-tauri/src/auth.rs:370`):**
- `heartbeat(api_url, access_token, node_id, tunnel_url, app_version)` sends POST to `/api/v1/node/heartbeat`
- Response already carries: `purge_directives`, `retention_challenges`, `storage_market`, `credit_balance`, `release_notes`, `mesh`
- Server-side handler: `GoodNewsEveryone/src/app/api/v1/node/heartbeat/route.ts`

**Operator Identity:**
- `wire_operators` table: `id (UUID)`, `email`, `credit_balance`
- `wire_agents` table has `operator_id UUID REFERENCES wire_operators(id)`
- `wire_nodes` table has `agent_id UUID REFERENCES wire_agents(id)`, `tunnel_url TEXT`
- Resolution chain: `wire_nodes.agent_id -> wire_agents.operator_id -> wire_operators.id`
- Node knows its own `operator_id` from registration (`auth.rs:27 — AuthState.operator_id`)

**HTTP Server Routes (`src-tauri/src/server.rs`):**
- Warp-based HTTP server with routes at `/health`, `/documents/{id}`, `/auth/callback`, `/auth/complete`, `/stats`, `/tunnel-status`, `/hooks/openrouter`
- Public HTML surface at `/p/` with its own auth middleware
- No `/v1/` prefixed routes exist yet on the node's HTTP server

**Dispatch Policy (`src-tauri/src/pyramid/dispatch_policy.rs`):**
- Contribution-governed routing rules: `DispatchPolicy.resolve_route(work_type, tier, step_name, depth) -> ResolvedRoute`
- `ResolvedRoute` contains ordered `Vec<RouteEntry>` provider preference chain
- Current routing is purely provider-based (ollama, openrouter). No fleet routing step exists yet.

**Frontend:**
- `MarketDashboard.tsx` — shell component with "Local Only" badge, "Coming Soon" roadmap
- `QueueLiveView.tsx` — real-time queue visualization (listens to `TaggedBuildEvent` bus)

---

## What to Build Tonight

### Wire Workstream (GoodNewsEveryone)

**1. Fleet roster in heartbeat response**

The heartbeat handler (`src/app/api/v1/node/heartbeat/route.ts`) must return a `fleet_roster` array containing all other online nodes belonging to the same operator.

After the credit balance lookup (~line 282), add a fleet roster query:

```typescript
// ── Fleet roster: same-operator nodes (for fleet-internal routing) ──
let fleetRoster: unknown[] = [];
try {
  // Resolve this node's operator_id
  const { data: thisAgent } = await adminClient
    .from('wire_agents')
    .select('operator_id')
    .eq('id', agent.id)
    .single();

  if (thisAgent?.operator_id) {
    // Find all OTHER online nodes under the same operator
    const { data: fleetNodes } = await adminClient
      .from('wire_nodes')
      .select('id, name, tunnel_url, wire_agents!inner(operator_id)')
      .eq('wire_agents.operator_id', thisAgent.operator_id)
      .eq('status', 'online')
      .not('id', 'eq', nodeId)  // exclude self
      .not('tunnel_url', 'is', null);  // must have tunnel

    fleetRoster = (fleetNodes ?? []).map((n: Record<string, unknown>) => ({
      node_id: n.id,
      name: n.name,
      tunnel_url: n.tunnel_url,
      // models_loaded will come from wire_compute_queue_state once nodes
      // push queue state. For now, fleet peers announce models directly.
    }));
  }
} catch (fleetErr) {
  // Never fail heartbeat due to fleet roster
  console.error('[node/heartbeat] fleet roster error:', fleetErr);
}
```

Add `fleet_roster: fleetRoster` to the `responseBody` object (~line 285).

**2. Fleet JWT issuance**

Add a new function to `src/lib/server/document-jwt.ts` (or create `src/lib/server/fleet-jwt.ts` — separate file is cleaner):

```typescript
// fleet-jwt.ts — JWT for fleet-internal authentication
import { SignJWT, jwtVerify, importPKCS8, importSPKI } from 'jose';

// Reuse the same Ed25519 keypair as document JWTs.
// Key loading identical to document-jwt.ts (copy the ensurePem + getPrivateKey + getPublicKey helpers,
// or extract to a shared key-loader module).

export interface FleetTokenPayload {
  /** Audience — always "fleet" */
  aud: string;
  /** Operator ID */
  op: string;
  /** Node ID of the bearer */
  nid: string;
  /** Expiration */
  exp: number;
  /** Issued at */
  iat: number;
}

export async function signFleetToken(payload: {
  op: string;
  nid: string;
  expiresInSeconds?: number;
}): Promise<string> {
  const privateKey = await getPrivateKey();
  const expiresIn = payload.expiresInSeconds ?? 3600; // 1 hour default

  return new SignJWT({ op: payload.op, nid: payload.nid })
    .setProtectedHeader({ alg: 'EdDSA' })
    .setAudience('fleet')
    .setIssuer('wire')
    .setIssuedAt()
    .setExpirationTime(`${expiresIn}s`)
    .sign(privateKey);
}
```

Issue the fleet JWT in the heartbeat response (alongside the fleet roster). Add to the heartbeat handler, inside the fleet roster block:

```typescript
// Issue fleet JWT (refreshed every heartbeat, 2h expiry for clock drift safety)
let fleetJwt: string | null = null;
if (thisAgent?.operator_id) {
  try {
    fleetJwt = await signFleetToken({
      op: thisAgent.operator_id,
      nid: nodeId,
      expiresInSeconds: 7200, // 2 hours — heartbeat interval is 60s, so always fresh
    });
  } catch (jwtErr) {
    console.error('[node/heartbeat] fleet JWT sign error:', jwtErr);
  }
}
```

Add `fleet_jwt: fleetJwt` to the `responseBody`.

**Also issue fleet JWT at registration time.** In `src/app/api/v1/node/register-with-session/route.ts`, after the response construction (~line 381), add `fleet_jwt` to the response body alongside the existing `jwt_public_key`. Resolve operator_id from the agent lookup that already exists in that handler.

### Node Workstream (agent-wire-node, Rust)

**3. Fleet roster storage**

Create `src-tauri/src/fleet.rs`:

```rust
// fleet.rs — Fleet roster and fleet dispatch client.
//
// Stores same-operator peer nodes discovered via heartbeat and direct
// fleet peer announcements. Provides fleet dispatch (POST to peer's
// tunnel) and fleet announce (POST to all peers on state change).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A fleet peer node (same operator, different hardware).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPeer {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,       // model IDs this peer has loaded
    pub queue_depths: HashMap<String, usize>, // model_id -> queue depth
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

/// Fleet roster — all known same-operator peers.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct FleetRoster {
    pub peers: HashMap<String, FleetPeer>,  // node_id -> peer
    pub fleet_jwt: Option<String>,          // Wire-signed JWT for fleet auth
    pub self_operator_id: Option<String>,   // this node's operator_id
}

impl FleetRoster {
    /// Update roster from heartbeat fleet_roster response.
    pub fn update_from_heartbeat(
        &mut self,
        peers: Vec<HeartbeatFleetEntry>,
        fleet_jwt: Option<String>,
    ) {
        // Merge — don't replace wholesale, because direct announcements
        // may have fresher data than the heartbeat snapshot.
        let now = chrono::Utc::now();
        for entry in peers {
            let peer = self.peers.entry(entry.node_id.clone()).or_insert_with(|| FleetPeer {
                node_id: entry.node_id.clone(),
                name: entry.name.clone(),
                tunnel_url: entry.tunnel_url.clone(),
                models_loaded: Vec::new(),
                queue_depths: HashMap::new(),
                last_seen: now,
            });
            // Heartbeat provides tunnel_url and name. Models come from
            // direct announcement (preferred) or queue state mirror.
            peer.tunnel_url = entry.tunnel_url;
            peer.name = entry.name;
            peer.last_seen = now;
        }
        if fleet_jwt.is_some() {
            self.fleet_jwt = fleet_jwt;
        }
    }

    /// Update from a direct fleet peer announcement.
    pub fn update_from_announcement(&mut self, announcement: FleetAnnouncement) {
        let now = chrono::Utc::now();
        let peer = self.peers.entry(announcement.node_id.clone()).or_insert_with(|| FleetPeer {
            node_id: announcement.node_id.clone(),
            name: announcement.name.clone().unwrap_or_default(),
            tunnel_url: announcement.tunnel_url.clone(),
            models_loaded: Vec::new(),
            queue_depths: HashMap::new(),
            last_seen: now,
        });
        peer.tunnel_url = announcement.tunnel_url;
        peer.models_loaded = announcement.models_loaded;
        peer.queue_depths = announcement.queue_depths;
        peer.last_seen = now;
        if let Some(name) = announcement.name {
            peer.name = name;
        }
    }

    /// Remove a peer (went offline).
    pub fn remove_peer(&mut self, node_id: &str) {
        self.peers.remove(node_id);
    }

    /// Find a fleet peer that has the given model loaded with queue
    /// capacity. Returns None if no peer qualifies.
    pub fn find_peer_for_model(&self, model_id: &str) -> Option<&FleetPeer> {
        let staleness_limit = chrono::Utc::now() - chrono::Duration::seconds(120);
        self.peers.values()
            .filter(|p| p.last_seen > staleness_limit)
            .filter(|p| p.models_loaded.contains(&model_id.to_string()))
            .min_by_key(|p| p.queue_depths.get(model_id).copied().unwrap_or(0))
    }
}

/// Shape of fleet roster entry from heartbeat response.
#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatFleetEntry {
    pub node_id: String,
    pub name: String,
    pub tunnel_url: String,
}

/// Shape of a direct fleet peer announcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetAnnouncement {
    pub node_id: String,
    pub name: Option<String>,
    pub tunnel_url: String,
    pub models_loaded: Vec<String>,
    pub queue_depths: HashMap<String, usize>,
    pub operator_id: String,
}

/// Shape of a fleet dispatch request (sent TO a peer node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchRequest {
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    pub fleet_jwt: String,
}

/// Shape of a fleet dispatch response (returned FROM a peer node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetDispatchResponse {
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub model: String,
    pub finish_reason: Option<String>,
}
```

Register the module in `lib.rs` — add `pub mod fleet;` alongside the existing `pub mod compute_queue;`.

**4. Fleet roster in AppState**

In `src-tauri/src/server.rs`, add to `ServerState`:

```rust
pub fleet_roster: Arc<RwLock<crate::fleet::FleetRoster>>,
```

In `main.rs`, construct it alongside the other state fields:

```rust
let fleet_roster = Arc::new(RwLock::new(crate::fleet::FleetRoster::default()));
```

Pass it into `ServerState` construction.

**5. Populate fleet roster from heartbeat**

In the heartbeat processing code (search for where the heartbeat response JSON is parsed — this is in `main.rs` or the background heartbeat task), extract `fleet_roster` and `fleet_jwt` from the response:

```rust
// After heartbeat response is parsed:
if let Some(fleet_array) = heartbeat_response.get("fleet_roster").and_then(|v| v.as_array()) {
    let entries: Vec<crate::fleet::HeartbeatFleetEntry> = fleet_array.iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    let jwt = heartbeat_response.get("fleet_jwt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut roster = fleet_roster.write().await;
    roster.update_from_heartbeat(entries, jwt);
}
```

**6. Fleet announce on startup and model change**

After tunnel connects and heartbeat succeeds, announce to all fleet peers:

```rust
// fleet.rs — add announce function:

/// Announce this node's state to all known fleet peers.
/// Called on startup, model load/unload, and going offline.
pub async fn announce_to_fleet(
    roster: &FleetRoster,
    self_announcement: &FleetAnnouncement,
) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let jwt = match &roster.fleet_jwt {
        Some(j) => j.clone(),
        None => {
            tracing::debug!("No fleet JWT, skipping fleet announce");
            return;
        }
    };

    for peer in roster.peers.values() {
        let url = format!("{}/v1/fleet/announce", peer.tunnel_url);
        let body = serde_json::to_value(self_announcement).unwrap_or_default();
        let jwt_clone = jwt.clone();
        let url_clone = url.clone();
        let peer_id = peer.node_id.clone();

        // Fire-and-forget per peer. Don't block on slow/dead peers.
        tokio::spawn(async move {
            match client
                .post(&url_clone)
                .header("Authorization", format!("Bearer {}", jwt_clone))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::debug!("Fleet announce to {} succeeded", peer_id);
                }
                Ok(resp) => {
                    tracing::warn!("Fleet announce to {} failed: {}", peer_id, resp.status());
                }
                Err(e) => {
                    tracing::warn!("Fleet announce to {} error: {}", peer_id, e);
                }
            }
        });
    }
}
```

Call `announce_to_fleet` in three places:
1. After first successful heartbeat that returns a fleet roster (in the heartbeat background task in `main.rs`)
2. When a model finishes loading in Ollama (search for model load completion in the Ollama control flow)
3. When the node is shutting down (with an `online: false` field in the announcement)

**7. Fleet dispatch client**

Add to `fleet.rs`:

```rust
/// Dispatch an LLM call to a fleet peer. Returns the response directly.
/// Timeout: 120s (LLM calls can be slow on large prompts).
pub async fn fleet_dispatch(
    peer: &FleetPeer,
    request: &FleetDispatchRequest,
) -> Result<FleetDispatchResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Fleet HTTP client error: {}", e))?;

    let url = format!("{}/v1/compute/fleet-dispatch", peer.tunnel_url);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", request.fleet_jwt))
        .header("Content-Type", "application/json")
        .json(request)
        .send()
        .await
        .map_err(|e| format!("Fleet dispatch to {} failed: {}", peer.node_id, e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Fleet dispatch to {} returned {}: {}", peer.node_id, status, text));
    }

    resp.json::<FleetDispatchResponse>()
        .await
        .map_err(|e| format!("Fleet dispatch response parse error: {}", e))
}
```

**8. Fleet routing in the LLM call path**

This is the critical integration point. Fleet routing must be checked BEFORE the local compute queue enqueue, but AFTER dispatch policy resolution.

In `src-tauri/src/pyramid/llm.rs`, in `call_model_unified_with_audit_and_ctx`, the compute queue interception is at ~line 626. Fleet routing must go BEFORE this:

```rust
// ── Fleet Routing: check fleet peers before local queue ───────
//
// If a fleet peer has this model loaded with capacity, dispatch
// to the peer instead of enqueueing locally. This happens before
// the compute queue check because fleet dispatch is transparent
// to the caller — same return type, same behavior.
//
// Only attempt fleet if:
// 1. Config has a fleet_roster (not tests/pre-init)
// 2. Caller is not the GPU loop (skip_concurrency_gate == false)
// 3. Provider is Ollama/local (fleet is for local models only)
// 4. Fleet roster has a peer with this model and capacity
if let Some(ref fleet_roster_handle) = config.fleet_roster {
    if !options.skip_concurrency_gate {
        let model_id = ctx
            .and_then(|c| c.resolved_model_id.clone())
            .unwrap_or_else(|| config.model.clone());

        let roster = fleet_roster_handle.read().await;
        if let Some(peer) = roster.find_peer_for_model(&model_id) {
            let jwt = roster.fleet_jwt.clone().unwrap_or_default();
            if !jwt.is_empty() {
                let request = crate::fleet::FleetDispatchRequest {
                    model: model_id.clone(),
                    system_prompt: system_prompt.to_string(),
                    user_prompt: user_prompt.to_string(),
                    temperature,
                    max_tokens,
                    response_format: response_format.cloned(),
                    fleet_jwt: jwt,
                };
                let peer_clone = peer.clone();
                drop(roster); // release read lock before async call

                match crate::fleet::fleet_dispatch(&peer_clone, &request).await {
                    Ok(fleet_resp) => {
                        // Convert FleetDispatchResponse to LlmResponse
                        return Ok(LlmResponse {
                            content: fleet_resp.content,
                            prompt_tokens: fleet_resp.prompt_tokens,
                            completion_tokens: fleet_resp.completion_tokens,
                            model: Some(fleet_resp.model),
                            finish_reason: fleet_resp.finish_reason,
                            ..Default::default()
                        });
                    }
                    Err(e) => {
                        // Fleet dispatch failed — fall through to local queue.
                        // This is expected (peer went offline, timeout, etc.)
                        tracing::warn!("Fleet dispatch failed, falling through to local: {}", e);
                    }
                }
            }
        }
    }
}

// ── Phase 1 Compute Queue: Transparent routing ─────────────── (existing code)
```

Add `fleet_roster` to `LlmConfig`:

```rust
// In LlmConfig struct (llm.rs):
pub fleet_roster: Option<Arc<tokio::sync::RwLock<crate::fleet::FleetRoster>>>,
```

Wire it through `LlmConfig` construction in `main.rs` / wherever LlmConfig is built, alongside the existing `compute_queue` field. Same pattern: set to Some in production, None in tests.

**9. Fleet dispatch receiving endpoint**

Add two new routes to the HTTP server in `server.rs`. These are under the `/v1/` prefix which does not currently exist, so create a new route group:

```rust
// In start_server(), after existing routes:

// ── Fleet endpoints (v1 prefix) ─────────────────────────────

// POST /v1/compute/fleet-dispatch — receive fleet LLM job from peer
let fleet_dispatch_route = {
    let state = state.clone();
    warp::path!("v1" / "compute" / "fleet-dispatch")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(warp::body::json())
        .and_then(move |auth_header: String, body: serde_json::Value| {
            let state = state.clone();
            async move {
                handle_fleet_dispatch(auth_header, body, state).await
            }
        })
};

// POST /v1/fleet/announce — receive fleet peer announcement
let fleet_announce_route = {
    let state = state.clone();
    warp::path!("v1" / "fleet" / "announce")
        .and(warp::post())
        .and(warp::header::<String>("authorization"))
        .and(warp::body::json())
        .and_then(move |auth_header: String, body: serde_json::Value| {
            let state = state.clone();
            async move {
                handle_fleet_announce(auth_header, body, state).await
            }
        })
};
```

Add these routes to the `.or()` chain with existing routes.

**10. Fleet dispatch handler**

```rust
async fn handle_fleet_dispatch(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Extract and verify fleet JWT
    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    let jwt_pk = state.jwt_public_key.read().await;
    if jwt_pk.is_empty() {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "No JWT public key configured"})),
            warp::http::StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    let claims = match verify_fleet_jwt(token, &jwt_pk) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Fleet dispatch JWT verification failed: {}", e);
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("JWT verification failed: {}", e)})),
                warp::http::StatusCode::FORBIDDEN,
            ));
        }
    };

    // 2. Verify same operator
    let self_operator_id = state.auth.read().await.operator_id.clone().unwrap_or_default();
    let jwt_operator_id = claims.op.unwrap_or_default();
    if self_operator_id.is_empty() || jwt_operator_id.is_empty() || self_operator_id != jwt_operator_id {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "Operator mismatch — not same fleet"})),
            warp::http::StatusCode::FORBIDDEN,
        ));
    }

    // 3. Parse request
    let model = body["model"].as_str().unwrap_or("").to_string();
    let system_prompt = body["system_prompt"].as_str().unwrap_or("").to_string();
    let user_prompt = body["user_prompt"].as_str().unwrap_or("").to_string();
    let temperature = body["temperature"].as_f64().unwrap_or(0.0) as f32;
    let max_tokens = body["max_tokens"].as_u64().unwrap_or(4096) as usize;
    let response_format = body.get("response_format").cloned();

    if model.is_empty() || user_prompt.is_empty() {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "Missing model or user_prompt"})),
            warp::http::StatusCode::BAD_REQUEST,
        ));
    }

    // 4. Enqueue in local compute queue (same queue as local builds)
    let compute_queue = {
        let pyramid = &state.pyramid;
        let config = pyramid.config.read().await;
        config.compute_queue_handle.clone()
    };

    // NOTE: The exact way to get the ComputeQueueHandle depends on where it
    // lives in the state. It may be on ServerState directly, or accessible
    // via the pyramid config's LlmConfig. Check current wiring.
    // The intent: build an LlmConfig with compute_queue: None (direct execution),
    // skip_concurrency_gate: true, and call through the GPU loop by enqueuing.

    // Build a minimal LlmConfig for this fleet job.
    // The config needs provider details for the local Ollama instance.
    // Clone the pyramid's base LlmConfig and override model + remove fleet/queue
    // to prevent re-dispatch loops.
    let mut fleet_config = state.pyramid.llm_config_base().await;
    fleet_config.model = model.clone();
    fleet_config.compute_queue = None;  // prevent re-enqueue
    fleet_config.fleet_roster = None;   // prevent fleet re-dispatch

    let (tx, rx) = tokio::sync::oneshot::channel();
    let options = crate::pyramid::llm::LlmCallOptions {
        skip_concurrency_gate: true,
        ..Default::default()
    };

    // Get the compute queue handle from wherever it lives in state.
    // Enqueue the fleet job into the same per-model FIFO queue.
    // This is the key integration: fleet jobs and local jobs share the queue.
    {
        let queue_handle = /* state.compute_queue or equivalent */;
        let mut q = queue_handle.queue.lock().await;
        q.enqueue_local(
            &model,
            crate::compute_queue::QueueEntry {
                result_tx: tx,
                config: fleet_config,
                system_prompt,
                user_prompt,
                temperature,
                max_tokens,
                response_format,
                options,
                step_ctx: None,  // Fleet jobs have no StepContext (not part of a local build)
                model_id: model.clone(),
                enqueued_at: std::time::Instant::now(),
            },
        );
        queue_handle.notify.notify_one();
    }

    // 5. Await result (synchronous — fleet dispatch blocks until GPU completes)
    match rx.await {
        Ok(Ok(llm_response)) => {
            let response = serde_json::json!({
                "content": llm_response.content,
                "prompt_tokens": llm_response.prompt_tokens,
                "completion_tokens": llm_response.completion_tokens,
                "model": llm_response.model,
                "finish_reason": llm_response.finish_reason,
            });
            Ok(warp::reply::with_status(
                warp::reply::json(&response),
                warp::http::StatusCode::OK,
            ))
        }
        Ok(Err(e)) => {
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("LLM call failed: {}", e)})),
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
        Err(_) => {
            Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "Queue channel closed"})),
                warp::http::StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    }
}
```

**11. Fleet JWT verification (node-side)**

Add to `server.rs`, alongside the existing `verify_jwt` and `verify_pyramid_query_jwt`:

```rust
/// JWT claims for fleet-internal authentication.
/// aud: "fleet", op: operator_id, nid: source node_id
#[derive(Debug, Deserialize)]
pub struct FleetJwtClaims {
    pub aud: Option<String>,
    #[serde(alias = "op")]
    pub op: Option<String>,
    pub nid: Option<String>,
    #[allow(dead_code)]
    pub exp: Option<u64>,
}

/// Verify a fleet JWT using Ed25519 public key.
/// Validates audience is "fleet". Returns claims for operator matching.
pub fn verify_fleet_jwt(
    token: &str,
    public_key_pem: &str,
) -> Result<FleetJwtClaims, String> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Invalid public key: {}", e))?;

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = true;
    validation.set_required_spec_claims(&["exp"]);
    validation.set_audience(&["fleet"]);

    let token_data = decode::<FleetJwtClaims>(token, &decoding_key, &validation)
        .map_err(|e| format!("Fleet JWT decode failed: {}", e))?;

    Ok(token_data.claims)
}
```

**12. Fleet announce handler**

```rust
async fn handle_fleet_announce(
    auth_header: String,
    body: serde_json::Value,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify fleet JWT
    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    let jwt_pk = state.jwt_public_key.read().await;

    let claims = match verify_fleet_jwt(token, &jwt_pk) {
        Ok(c) => c,
        Err(e) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("JWT verification failed: {}", e)})),
                warp::http::StatusCode::FORBIDDEN,
            ));
        }
    };

    // 2. Verify same operator
    let self_operator_id = state.auth.read().await.operator_id.clone().unwrap_or_default();
    let jwt_operator_id = claims.op.unwrap_or_default();
    if self_operator_id.is_empty() || self_operator_id != jwt_operator_id {
        return Ok(warp::reply::with_status(
            warp::reply::json(&serde_json::json!({"error": "Operator mismatch"})),
            warp::http::StatusCode::FORBIDDEN,
        ));
    }

    // 3. Parse announcement and update roster
    let announcement: crate::fleet::FleetAnnouncement = match serde_json::from_value(body) {
        Ok(a) => a,
        Err(e) => {
            return Ok(warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": format!("Invalid announcement: {}", e)})),
                warp::http::StatusCode::BAD_REQUEST,
            ));
        }
    };

    {
        let mut roster = state.fleet_roster.write().await;
        roster.update_from_announcement(announcement);
    }

    Ok(warp::reply::with_status(
        warp::reply::json(&serde_json::json!({"status": "ok"})),
        warp::http::StatusCode::OK,
    ))
}
```

### Frontend Workstream

**13. Fleet status in Market tab**

Update `src/components/MarketDashboard.tsx` to show fleet peer status:

```tsx
// Add fleet roster to the IPC calls. The fleet roster is readable
// from the backend via a new Tauri command.

// In MarketDashboard.tsx:
// - Add a "Fleet Nodes" section showing:
//   - Peer name, tunnel status (green dot = online/fresh, grey = stale)
//   - Models loaded on each peer
//   - Queue depth per model on each peer
// - "Local Only" badge changes to "Fleet Active (N peers)" when fleet roster is non-empty
```

Add a Tauri IPC command to expose fleet roster to frontend:

```rust
// In main.rs or commands.rs:
#[tauri::command]
async fn get_fleet_roster(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let roster = state.fleet_roster.read().await;
    serde_json::to_value(&*roster).map_err(|e| e.to_string())
}
```

**14. Queue view fleet job indicators**

In `QueueLiveView.tsx`, fleet-dispatched jobs (received from peers) should show a fleet badge. This requires the GPU loop to emit a `QueueJobStarted` event with a `source: "fleet"` field.

In the GPU loop (main.rs ~line 11298), when emitting `QueueJobStarted`, include whether the entry has a StepContext or not. Fleet jobs have `step_ctx: None`, local builds have `step_ctx: Some(...)`. Use this as the discriminator:

```rust
// In TaggedKind enum (event_bus.rs), extend QueueJobStarted:
QueueJobStarted {
    model_id: String,
    source: String,  // "local" | "fleet"
},
```

Set `source: if entry.step_ctx.is_some() { "local" } else { "fleet" }` in the GPU loop's event emission.

---

## Critical Rules

Read BEFORE implementing:
- `/Users/adamlevine/AI Project Files/agent-wire-node/docs/SYSTEM.md` — The Five Laws
- `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/wire-pillars.md` — The 44 Pillars
- **Law 1 (One Executor):** Fleet jobs enter the SAME compute queue as local jobs. One queue, one GPU loop, one executor. No separate fleet executor.
- **Law 4 (StepContext):** Fleet jobs arriving from a peer DO NOT have a StepContext (they are not part of a local build). `step_ctx: None` is correct for fleet-received jobs. The DISPATCHING side's StepContext stays on the dispatching node (the fleet dispatch is opaque — it returns an LlmResponse, and the local StepContext wraps that).
- **Pillar 37 (No hardcoded numbers):** The 120s fleet dispatch timeout should be a contribution, not a constant. But for v1, a constant is acceptable if it's clearly marked with `// TODO: contribution-driven` and defined in one place.

## Build Pattern

1. **Wire implementer** — heartbeat fleet roster + fleet JWT issuance. Small scope.
2. **Node implementer** — fleet.rs, fleet routing in llm.rs, fleet endpoints in server.rs. This is the bulk of the work.
3. **Frontend implementer** — fleet status in MarketDashboard, queue source indicators.
4. **Serial verifier+fixer** — same instructions. Arrives expecting to build, audits instead, fixes in place.
5. **Wanderer** — "Fleet routing between two nodes under the same operator — does this actually work?" Traces: heartbeat -> fleet roster -> fleet announce -> dispatch -> queue -> GPU -> response.

## What NOT to Build Tonight

- **Exchange/matching** — no order book, no `wire_compute_offers`, no `match_compute_job` RPC. Fleet bypasses all of this.
- **Settlement/credits** — fleet is free. No `settle_compute_job`, no deposits, no reservation fees.
- **Relay network** — fleet is direct peer-to-peer. No relay hops.
- **Queue mirroring to Wire** — fleet traffic is invisible to the Wire. No `wire_compute_queue_state` updates for fleet jobs.
- **Competitive pricing** — fleet has no pricing. No `compute_pricing` contributions needed.
- **Bridge mode** — no OpenRouter-to-network bridging. Fleet is local hardware only.
- **Queue discount curves** — no pricing = no discounts.
- **Network observations** — fleet jobs do NOT produce `wire_compute_observations`. The Wire never sees them. (Optional future enhancement: node can self-report fleet performance for its own profile.)
- **Privacy/relay tiers** — fleet is inherently private (same operator's hardware, direct tunnel).
- **Cancel/void/fail RPCs** — no Wire involvement means no Wire job lifecycle.
- **Speculative reservation** — fleet dispatch is immediate, no reservation needed.

## Critical Implementation Details

### Routing Order

The full routing order in `call_model_unified_with_audit_and_ctx` is:

1. **Fleet check** — if fleet roster has a peer with model + capacity, dispatch to peer
2. **Compute queue** — if fleet unavailable or failed, enqueue in local per-model queue
3. (Future) **Market exchange** — if local queue is full and market is enabled

Fleet failure (timeout, HTTP error) falls through to local. Never retry fleet — the local queue IS the fallback. The fleet roster's staleness window (120s) means occasionally dispatching to a node that just went offline. The 120s HTTP timeout catches this.

### Fleet JWT Lifetime and Refresh

- Fleet JWT expires in 7200s (2 hours)
- Heartbeat interval is 60s
- Every heartbeat response carries a fresh fleet JWT
- The node stores the latest JWT in `FleetRoster.fleet_jwt`
- If the JWT is expired when a fleet dispatch is attempted, skip fleet (fall through to local)
- No explicit refresh — the heartbeat refresh cycle handles it

### Tunnel URL Format

Tunnel URLs look like: `https://{random-subdomain}.cfargotunnel.com`

The node's HTTP server runs on `localhost:PORT` (search for `warp::serve` in `server.rs`). The Cloudflare tunnel maps the public URL to `localhost:PORT`. So `POST https://{subdomain}.cfargotunnel.com/v1/compute/fleet-dispatch` hits the node's warp server at `localhost:PORT/v1/compute/fleet-dispatch`.

### ComputeQueueHandle Access from Fleet Handler

The fleet dispatch handler needs the `ComputeQueueHandle` to enqueue fleet jobs. Two options:

**Option A (preferred):** Add `compute_queue: ComputeQueueHandle` to `ServerState`. It's already an `Arc`-wrapped clone. The fleet handler enqueues directly.

**Option B:** Build an LlmConfig with `compute_queue: Some(handle)` and `fleet_roster: None`, then call `call_model_unified_with_audit_and_ctx`. This goes through the transparent queue path. Simpler but adds one level of indirection. Choose this if the LlmConfig construction is easier to wire up.

### Models Loaded Discovery

The fleet announce includes `models_loaded`. The node needs to know what models IT has loaded to send in its announcements. Search for how the current code tracks loaded models — this is in the Ollama control plane / local mode state. The `models_loaded` list should be read from whatever struct holds the current Ollama model state.

### LlmResponse Default

The fleet dispatch code does `..Default::default()` on `LlmResponse`. Verify that `LlmResponse` derives or implements `Default`. If it does not, add `#[derive(Default)]` or implement it manually with empty/None fields.

### No Concurrent Fleet + Local for Same Call

A single LLM call either goes to fleet OR local, never both. The fleet check is sequential — if fleet peer exists and dispatch succeeds, return immediately. If fleet dispatch fails, fall through to local queue. No speculative dispatch to both.

## Known Gotchas

1. **LlmConfig clone cost.** `LlmConfig` has many fields (credential store, provider registry, etc.). The fleet dispatch clones it to set `compute_queue: None`. This is the same pattern the existing queue code uses (~line 638 in llm.rs). Verify this doesn't clone heavy Arc'd state by value.

2. **Fleet roster staleness.** If a peer goes offline between heartbeats, the roster still lists them for up to 120s. Fleet dispatch to a dead peer will timeout (120s HTTP timeout). This is a 120s penalty per failed fleet dispatch. Mitigation: on fleet dispatch failure, remove the peer from the roster immediately so subsequent calls don't retry the dead peer.

3. **`warp::path!` macro and route ordering.** The new `/v1/compute/fleet-dispatch` and `/v1/fleet/announce` routes must not conflict with existing routes. Since no `/v1/` routes exist yet, there's no conflict. But they MUST be `.or()`'d into the route chain before the catch-all 404 or the public HTML surface.

4. **Tokio runtime for fleet dispatch.** The fleet dispatch handler awaits the oneshot receiver, which blocks until the GPU loop processes the item. This is correct — the HTTP response waits for the LLM result. But if many fleet jobs arrive simultaneously, each holds a warp handler task open. This is fine because the GPU loop serializes execution anyway, and warp's task pool handles the waiting.

5. **Fleet JWT audience collision.** The existing JWT verification functions validate specific audiences (`"pyramid-query"` for pyramid queries, none for document tokens). The new fleet JWT uses `aud: "fleet"`. The `verify_fleet_jwt` function validates this explicitly. Make sure the fleet endpoints use `verify_fleet_jwt`, NOT the existing `verify_jwt` or `verify_pyramid_query_jwt`.

6. **AuthState.operator_id availability.** The fleet dispatch handler reads `state.auth.read().await.operator_id` to verify same-operator. This field is populated during `register_with_session` (auth.rs:28). If the node hasn't registered yet (pre-auth state), `operator_id` is None and all fleet dispatch attempts are rejected. This is correct — no fleet before registration.

7. **Heartbeat response parsing.** The node-side heartbeat function (`auth.rs:370`) returns `serde_json::Value`. The caller (in `main.rs` background task) must parse the new `fleet_roster` and `fleet_jwt` fields. Search for where `heartbeat()` return value is consumed and add the fleet roster extraction there.

8. **No fleet dispatch for OpenRouter models.** Fleet routing is for local Ollama models only. If the dispatch policy resolves to OpenRouter, fleet should not be attempted. The fleet roster only contains Ollama model IDs (local models). The `find_peer_for_model` check naturally excludes OpenRouter models because peers don't advertise them.
