# Handoff: Wire-side work for compute market Phase 2 + Phase 3

**To:** Whoever is working in the `GoodNewsEveryone` (Wire) repo.
**From:** Node-side Phase 2 implementer (agent-wire-node branch `feat/compute-market-phase-2`, commit `609b895` or later).
**Goal:** enable tester onboarding — friends install agent-wire-node, get seeded with credits, build pyramids that use compute from the market (either from Adam's GPU or, eventually, from each other).

**This handoff has two parts:**
1. **What the node needs from the Wire** — functional requirements, not implementation prescriptions. Wire has its own idioms; use them. If something here is protocol-level wrong, we change the protocol.
2. **What the Wire needs from the node** — requests for canonical schemas + decisions I need from you to finish Phase 3 node-side.

---

## Context: what the node ships today

Branch `feat/compute-market-phase-2` in `adamlevineagent/agent-wire-node` is Phase 2 provider-side complete, including:
- Receives `POST /v1/compute/job-dispatch` from the Wire, verifies `wire_job_token` JWT (aud=`compute`, pid=self_node_id, sub=job_id), runs inference, writes result to outbox
- Publishes offers via Tauri IPC → `POST /api/v1/compute/offers` (node expects Wire route to exist)
- Pushes queue state to Wire via `POST /api/v1/compute/queue-state` on debounced nudge
- Outbox + sweep + worker heartbeat primitives all shipped and tested
- DADBEAR work items created with `slug="market:compute"` for every market job, with admission hold check (blocking holds from DD-H enumerated)
- Chronicle events: `market_offered`, `market_received`, `queue_mirror_push_failed` all firing

Node-side Phase 3 (requester side + outbox delivery worker) is **not yet built** but is designed. The primitives it needs are in place on the node; it just needs something to talk to.

---

## Part 1: What the node needs from the Wire

### 1.1 Canonical JSON shapes we're expecting

These are what the node sends/receives today. **Request: confirm or correct these shapes.** If Wire uses different field names or nested structure, tell us and we'll adapt node-side serde.

**Offer upsert: `POST /api/v1/compute/offers`**

Node sends (body):
```json
{
  "model_id": "gemma3:27b",
  "provider_type": "local",
  "rate_per_m_input": 100,
  "rate_per_m_output": 500,
  "reservation_fee": 10,
  "queue_discount_curve": [
    {"depth": 0, "multiplier_bps": 10000},
    {"depth": 4, "multiplier_bps": 9500}
  ],
  "max_queue_depth": 8
}
```

Expected response:
```json
{ "offer_id": "<wire-assigned uuid>", "status": "active" }
```

Auth: node's operator API token (Bearer). UPSERT semantics per `UNIQUE(node_id, model_id, provider_type)`.

**Offer delete: `DELETE /api/v1/compute/offers/{offer_id}`**

Path: URL-encoded offer_id. 404 = ok (idempotent). Node uses this when operator removes an offer. Active jobs continue; only new matches are prevented.

**Market surface: `GET /api/v1/compute/market-surface[?model_id=X]`**

Expected response shape (guessed from the plan; correct as needed):
```json
{
  "fetched_at": "2026-04-17T12:00:00Z",
  "models": [
    {
      "model_id": "gemma3:27b",
      "providers": [
        { "node_id": "...", "provider_type": "local", "rate_per_m_input": 100,
          "rate_per_m_output": 500, "reservation_fee": 10, "queue_depth": 3,
          "max_queue_depth": 8, "median_tps": 120.5, "p95_latency_ms": 2400,
          "observation_count": 42 }
      ],
      "min_rate_input": 100, "max_rate_input": 150,
      "min_rate_output": 500, "max_rate_output": 700,
      "total_queue_depth": 12, "provider_count": 3,
      "median_tps": 118.0, "p95_latency_ms": 2500
    }
  ]
}
```

**Queue state push: `POST /api/v1/compute/queue-state`**

Node sends (body), debounced ~500ms (configurable via `market_delivery_policy.queue_mirror_debounce_ms`):
```json
{
  "node_id": "...",
  "models": [
    {
      "model_id": "gemma3:27b",
      "market_depth": 2,
      "total_depth": 5,
      "seq": 42,
      "is_executing": true,
      "est_next_available_s": 120,
      "max_market_depth": 8,
      "max_total_depth": 16
    }
  ]
}
```

**Privacy J7:** `local_depth` and `executing_source` are deliberately NOT included. Wire should reject pushes that try to include them (or just ignore extra fields; node serializer won't send them).

Wire stores this; returns 2xx on accept, 409 on stale seq (node logs, next nudge re-pushes).

**Match RPC wrapper: `POST /api/v1/compute/match`** (Phase 3 node will call)

Node sends:
```json
{
  "model_id": "gemma3:27b",
  "max_budget": 1000,
  "input_tokens": 2048,
  "latency_preference": "fast"
}
```

Expected response (per `compute-market-phase-2-exchange.md` §II API Routes item 2):
```json
{
  "job_id": "<uuid>",
  "matched_rate_in": 100,
  "matched_rate_out": 500,
  "matched_multiplier_bps": 9500,
  "reservation_fee": 10,
  "estimated_deposit": 250,
  "queue_position": 3
}
```

**Privacy J7 on this path too:** `provider_tunnel_url` MUST NOT be returned. Wire does the dispatch internally; never expose provider identity to the requester in Phase 2.

**Fill RPC wrapper: `POST /api/v1/compute/fill`** (Phase 3 node will call)

Node sends:
```json
{
  "job_id": "<uuid from match>",
  "input_token_count": 2048,
  "temperature": 0.2,
  "max_tokens": 1024,
  "messages": [
    {"role": "system", "content": "..."},
    {"role": "user", "content": "..."}
  ],
  "relay_count": 0
}
```

Expected response:
```json
{
  "deposit_charged": 250,
  "relay_chain": [],
  "provider_ephemeral_pubkey": null,
  "total_relay_fee": 0
}
```

`relay_chain` is `[]` and `provider_ephemeral_pubkey` is null until relay market ships. Reject `relay_count > 0` at launch per DD-C.

**Important:** `fill_compute_job` on the Wire side must mint a `wire_job_token` JWT with these claims (per DD-F):
```
aud = "compute"
iss = Wire's signing key identifier (same as fleet)
exp = now + fill_job_ttl_secs (default 300s; read from economic_parameter contribution)
iat = now
sub = job_id (UUID string)
pid = provider_node_id (the matched provider's node_id)
```

Signing algorithm: Ed25519 (same key the fleet JWTs use). Node-side verifier is already shipped: `pyramid/market_identity.rs::verify_market_identity`.

**Bootstrap-mode callback target: `POST /api/v1/compute/result-relay`**

Per the SOTA privacy model (architecture §III), at launch the Wire acts as a transient relay. Every `MarketDispatchRequest.callback_url` points here. When a provider POSTs the result envelope, the Wire:
1. Verifies the JWT on the POST
2. Looks up the original requester (by `sub` = job_id → `requester_node_id` on `wire_compute_jobs`)
3. Forwards the result to the real requester (via whatever mechanism Wire uses — webhook, queue, direct POST, etc.)
4. Triggers settlement on delivery

Node POSTs to this endpoint with `Authorization: Bearer <wire_job_token>` and body:
```json
{
  "job_id": "<uuid>",
  "outcome": {
    "kind": "Success",
    "data": {
      "content": "...",
      "prompt_tokens": 512,
      "completion_tokens": 128,
      "model": "gemma3:27b",
      "finish_reason": "stop",
      "provider_model": "gemma3:27b"
    }
  }
}
```

or for error:
```json
{
  "job_id": "<uuid>",
  "outcome": { "kind": "Error", "data": "worker heartbeat timeout" }
}
```

Tagged-enum shape mirrors `FleetAsyncResult`. The node's outbox delivery worker (Phase 3) will POST here repeatedly with backoff until 2xx.

### 1.2 Settlement triggers

Per plan, the Wire auto-settles when a result is delivered. Node doesn't call a settle RPC. But it would be useful for observability if:

- A **chronicle-like signal** flows from Wire back to node: "your job X settled; you earned Y credits; here's the reason for any discrepancy."
- Currently the node doesn't know the outcome of settlement on a served job except via the result delivery POST success/failure.

**Request:** think about whether there's a way to stream settle events back to providers. A Webhook pointed at `POST /v1/compute/settle-notify` on the node? A polled endpoint the node can call to reconcile? Your call — just needs to exist so `ComputeJob.status` in the node's state can move past `Ready` and into a proper terminal `Settled`/`Voided` state.

### 1.3 Credit granting flow (for tester seeding)

Adam's plan: "give testers a bunch of credits so they can use the market." Requests:

1. **What's the existing flow?** Is there an admin endpoint Adam already uses to grant credits to an operator_id? If yes, point me at it — the node's credits UI can surface it.
2. **If not, what's the minimum shape?** Probably `POST /api/v1/admin/credits/grant { operator_id, amount, reason }` gated by a superuser role. Or a Supabase SQL runbook.
3. **How do credits flow through settlement?** When a requester's job settles, does the Wire debit their credit pool and credit the provider's? What's the unit? Are credits 1:1 dollars or a separate token?
4. **Can credits be time-limited / capped?** For testers, Adam probably wants to cap how much any single tester can spend (e.g., $5-worth of inference over the test period) to prevent runaway builds eating his budget.

Even rough answers here unblock the rest.

### 1.4 Schemas I'm guessing at — please confirm or redirect

The node holds its own view of these; Wire holds the truth. Please share your canonical shapes:

- **`wire_compute_offers` table columns + indices.** Specifically: is `queue_discount_curve` stored as JSONB? What's the UNIQUE constraint exact shape?
- **`wire_compute_jobs` table** — status enum values + transitions: `matched → filled → executing → ready → delivered → settled/failed/voided`? Is this the canonical state machine?
- **`wire_compute_queue_state` table** — exact columns + PK shape. Per-node-per-model with a composite PK? Per-node with a JSONB models field?
- **`wire_compute_observations` table** — what observations are recorded? TPS, latency, token counts? Node-reported or Wire-derived?
- **`market_rotator`** state table — the 76/2/2 Bjorklund distribution for provider/Wire/GraphFund splits. Is this live or stubbed?
- **Economic parameters the Wire reads per-call:** `fill_job_ttl_secs`, `max_completion_token_ratio`, `default_output_estimate`. Do these exist as `wire_contributions.type = 'economic_parameter'` rows? If yes, are the values the ones the node-side plan specs (300s TTL, 2x ratio, 500 default estimate)?

### 1.5 Protocol-level questions (100-year correctness, flagged per Adam's ask)

None of these block Phase 2. But since Adam said "if the protocol is wrong, we'll change it," here are the places I'd want another pair of eyes:

**Q-PROTO-1: Outbox table reuse vs. separate market_result_outbox.**
Per DD-D, market dispatches reuse `fleet_result_outbox` with `callback_kind` discriminator + `WIRE_PLATFORM_DISPATCHER` sentinel for `dispatcher_node_id`. This works but is architecturally hacky — it overloads a table whose PK shape assumes "peer-to-peer within one operator." For 100-year correctness, markets might deserve their own `market_result_outbox` with a proper `(requester_operator_id, job_id)` PK and no sentinels. The downside: ~14 CAS helpers currently shared. Your call whether to push back on DD-D; I can unwind if you want.

**Q-PROTO-2: Should `wire_job_token` JWT bind `callback_url`?**
Currently the JWT has `aud, iss, exp, iat, sub, pid` (per DD-F). The `callback_url` is in the dispatch body but NOT a signed claim. A compromised Wire (or someone with the signing key) could mint a valid JWT with a hostile callback_url — provider would POST the result to an attacker's server. If we added `cb_url_hash` as a JWT claim (SHA-256 of the normalized callback_url), the provider could verify destination authenticity.

Trade-off: JWT size grows slightly; rotation/tunnel changes mid-flight would invalidate the token. Phase 3 question — defer if you want, but flag for consideration.

**Q-PROTO-3: `privacy_tier` as string vs enum.**
Currently `"standard" | "cloud_relay"` as a String. Bounded set. Enum would be strictly better on both ends. Easy to change now, harder after thousands of stored jobs. Small question.

**Q-PROTO-4: Bootstrap-mode "Wire as relay" vs. direct callback.**
The SOTA model per architecture §III uses the Wire as transient bootstrap relay — `callback_url` always points at `{wire_base}/v1/compute/result-relay` at launch, shifts to requester tunnels / relay chains post-relay-market.

Question: is the "callback_url value over time" protocol clear on your end? Specifically: when does the shift happen? Per-request based on requester's `allow_relay_usage` setting? Per-deployment-phase? A global flag?

**Q-PROTO-5: What's the Wire doing with `max_market_depth` stored on `wire_compute_offers`?**
Node has its own per-offer cap. Wire presumably has one too (for admission throttling). If they disagree, whose wins? The spec says Wire does match-time admission checks against `max_queue_depth`, but node also enforces at `/v1/compute/job-dispatch` admission. Double-gate is fine; just want to know you're intentionally doing it.

---

## Part 2: What the Wire needs from the node

### 2.1 Documents to read

Node-side Phase 2 is documented across these files on branch `feat/compute-market-phase-2`:
- `docs/plans/compute-market-architecture.md` §VIII.6 (DD-series decisions) — the load-bearing design log
- `docs/plans/compute-market-phase-2-exchange.md` — node-side handler + worker spec
- `docs/plans/handoff-2026-04-17-compute-market-phase-2-shipped.md` — what shipped, in what order, with test counts

All pushed to `origin/feat/compute-market-phase-2`.

### 2.2 Shapes the Wire can consume

Node's Rust types for shared envelopes:
- `MarketDispatchRequest` (Wire → provider body): `src-tauri/src/pyramid/market_dispatch.rs`
- `MarketAsyncResultEnvelope` (provider → Wire body via result-relay): same file
- `MarketClaims` JWT shape: `src-tauri/src/pyramid/market_identity.rs`
- `MarketDispatchAck` (202 body from provider): same file as request
- Queue state snapshot: `src-tauri/src/pyramid/market_mirror.rs` around the push logic

All use serde. If you want me to extract them as standalone JSON schemas for the Wire-side TypeScript/Zod, ping me.

### 2.3 Node can do local work ahead of Wire landing

While the Wire endpoints get built, node-side Phase 3 can land in parallel:
- Outbox delivery worker (POSTs to callback_url; works against `/v1/compute/result-relay` once Wire ships it)
- Requester-side dispatch in `llm.rs` Phase A (calls `/api/v1/compute/match` + `/fill`)
- `PendingMarketJobs` population + oneshot plumbing

Both sides can be written against the spec, then integration-test together.

---

## Part 3: Appendix — Node CLI gap (not Wire's problem, but needs fixing)

This is the "agents can't drive the node" gap Adam raised. Not Wire scope — flagging because it matters for how tester onboarding works in practice. If the tester flow relies on a human clicking, agents can't help seed/onboard/support.

**Fix shape (node-side, planned):** add HTTP routes mirroring the Tauri IPCs, auth-gated by the node's own API token. Then `curl` works, and a future `wire-node-cli` is a thin wrapper.

**Minimum route set (all under `/v1/node/` or similar, bearer-auth):**

```
# Market
POST   /v1/node/market/offers          (wraps compute_offer_create)
PUT    /v1/node/market/offers/{id}     (wraps compute_offer_update)
DELETE /v1/node/market/offers/{id}     (wraps compute_offer_remove)
GET    /v1/node/market/offers          (wraps compute_offers_list)
GET    /v1/node/market/surface         (wraps compute_market_surface)
POST   /v1/node/market/enable          (wraps compute_market_enable)
POST   /v1/node/market/disable         (wraps compute_market_disable)
GET    /v1/node/market/state           (wraps compute_market_get_state)

# Participation policy
GET    /v1/node/policy/participation   (get current)
PUT    /v1/node/policy/participation   (supersede)

# Pyramids (probably already exist)
POST   /v1/node/builds                  (start a build)
GET    /v1/node/builds/{slug}           (status)
DELETE /v1/node/builds/{slug}           (cancel)

# Fleet
GET    /v1/node/fleet/peers
GET    /v1/node/fleet/identity

# Health
GET    /v1/node/health
GET    /v1/node/tunnel
```

Tauri IPC handlers become thin wrappers over the same Rust functions. Route handlers and IPC handlers share the body.

**Alternative:** build `wire-node-cli` as a separate binary that talks to a daemon mode of `wire-node-desktop`. More work; worse composition. The HTTP-first approach wins unless there's a reason not to.

**Recommendation for Wire team:** doesn't affect you directly, but if you want to drive a tester's node for support (e.g., "run this offer for them"), the HTTP routes above will be the surface.

---

## Summary — what I need from you to unblock

**Blocking node Phase 3 (estimated prio):**
1. HTTP routes for `/api/v1/compute/match` + `/fill` + `/result-relay` — without these, requester-side node code has nothing to call.
2. Confirmation of canonical shapes for request/response bodies so node serde stays in sync.
3. JWT signing: confirm Wire is minting `MarketClaims` with the exact fields per DD-F (aud, iss, exp, iat, sub, pid) using the same Ed25519 key as fleet.
4. Credit granting flow — however you want to do it, just let Adam seed tester operators.

**Blocking node Phase 2 end-to-end testability:**
5. HTTP routes for `/api/v1/compute/offers` (CREATE + DELETE), `/market-surface`, `/queue-state`. Without these, the node UI I just shipped 404s on every call. First offer publish is the smoke test.

**Nice-to-have (Phase 3 polish):**
6. Settlement-notify signal back to the node so `ComputeJob.Ready → Settled` transitions are observable.

**Decisions requested (flag any you want to change):**
7. Q-PROTO-1 through Q-PROTO-5 above. I'll go with the current design unless you push back.

Drop any of the above into the node's plan docs as canonical and we'll adapt node-side. If something in the plan docs is wrong because the Wire implementation differs, the plan docs lose.

---

**Contact:** coordinate through Adam. Node-side agent is on branch `feat/compute-market-phase-2`, happy to pair on any of the above.
