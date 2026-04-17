# Node Phase 3 — Requester-Side Compute Market

**Status:** Plan, rev 0.3. Ready to implement the moment Wire ships W3+W4 paired on dev.
**Scope:** Node-side only. Teaches `pyramid_build` (and its siblings) to dispatch inference through the compute market instead of always hitting local/OpenRouter.
**Dependencies:** Wire W3+W4 paired (W3 = `/match` + `/fill` + wire_job_token JWT mint + dispatch orchestration; W4 = `POST /api/v1/compute/callback/:job_id` + delivery worker pushing results to requester_callback_url + `GET /api/v1/compute/jobs/:job_id` status poll).
**Contract reference:** `GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md` **rev 1.4** (Wire→Requester delivery path in §2.5, JWT auth via `aud=result-delivery`).
**W3 spec:** `GoodNewsEveryone/docs/plans/compute-market-W3-spec.md` rev 0.3 (post-Option-Y lock).
**Estimated scope:** ~3–4h realistic.

> **Heads-up for morning-me / future readers:** this plan was rewritten 2026-04-17 late-night to reflect the Option-Y / push-primary architecture after a live handshake with the Wire owner. Previous revisions framed Phase 3 as "polling-only with push deferred to 3.5" — that was wrong and impossible (`GET /jobs/:job_id` returns status only, never content; content flows ONLY via the push path). Current plan makes push primary with polling as a timeout fallback. All changes documented in the rev log at the bottom.

---

## 1. What Phase 3 actually is

Phase 2 shipped the **provider side**: this node receives dispatches from Wire, runs inference, POSTs results back.

Phase 3 ships the **requester side**: this node, when building a pyramid, **asks the market for inference instead of (or in addition to) using local Ollama / OpenRouter**.

The payoff: a tester's 200-node L0 cluster build that takes 75 minutes locally takes ~45 seconds on the network, because 184 other nodes run 184 of the 200 inference calls concurrently.

### 1.1 Three endpoints + one inbound webhook

The requester flow is four-beat. Three Wire endpoints the node calls + one node endpoint Wire calls:

1. **`POST /api/v1/compute/match`** (client) — ask Wire to find a provider. Returns `{job_id (handle-path), request_id, matched_rate_in/out_per_m, matched_multiplier_bps, reservation_fee, estimated_deposit, queue_position}`. Wire has debited a reservation from requester balance. `request_id` will be used as the Idempotency-Key on /fill.
2. **`POST /api/v1/compute/fill`** with `Idempotency-Key: <request_id>` (client) — Wire mints the `wire_job_token` JWT (aud=compute), generates the provider-side callback bearer, POSTs `MarketDispatchRequest` to the provider's tunnel, returns 200 ACK with `{status: "dispatched", job_id, provider_node_id, peer_queue_depth, deposit_charged, estimated_output_tokens, dispatch_timeout_ms}`. Node body includes `requester_callback_url` — a full HTTPS URL on this node's tunnel where Wire's delivery worker will push the result.
3. **`POST <node_tunnel>/v1/compute/job-result`** (server — NEW in Phase 3) — Wire's delivery worker pushes the result envelope here when the provider completes and Wire's transit table has the content. Auth: Wire-signed JWT with `aud=result-delivery` (see §4.2).
4. **`GET /api/v1/compute/jobs/:job_id`** (client, fallback only) — status-only poll used as timeout sentinel. Returns `{status, tokens, latency, outcome_kind, delivery_status}` — **never content**. Node uses this ONLY to detect when push has failed terminally so it can fall back to local inference.

### 1.2 Push is primary. Polling is a fallback sentinel.

Contract §2.4 is explicit: **Wire does NOT persist result content.** Content lives in `wire_compute_result_transit` for a 1-hour TTL; the only way it leaves the transit table is via Wire's delivery worker pushing to `requester_callback_url`. `GET /jobs/:job_id` returns status + token counts + outcome — never content.

This means a polling-only requester receives `status: "completed"` + `output_tokens: 384` and zero content. Useless for `pyramid_build`, which needs the content to feed into the next build step.

So Phase 3 hosts the inbound receiver at `POST /v1/compute/job-result`. The `pending_jobs` map + oneshot channel plumbing wakes the awaiting inference call when Wire pushes. Polling is only used when push fails terminally (5 retry attempts exhausted per `compute_delivery_policy`), at which point `GET /jobs/:job_id` returns `delivery_status: "failed"` and the node surfaces a timeout to the caller → fallback to local inference.

**Phase 3 = push primary. Polling = one call, only when push has been declared failed by Wire.**

---

## 2. Where it slots into the node's existing inference paths

### 2.1 The inference fan-out lives at `call_model_unified`

Every LLM call in the build pipeline eventually routes through `call_model_unified` (in `src-tauri/src/pyramid/llm.rs`). It already has tier dispatch (primary / fallback_1 / fallback_2) + provider dispatch (openrouter / ollama-local / bridge-tier / dispatched-to-fleet). Adding a **market dispatch branch** is the clean extension point.

### 2.2 When does a call go to market?

Per `compute_participation_policy`:
- `allow_market_dispatch = true` AND
- current balance ≥ estimated deposit AND
- `/match` returns a provider in reasonable queue position (configurable threshold)

If any fail, fall through to existing provider dispatch (local / OpenRouter). Market is an **additive** path, not a replacement.

### 2.3 Policy knobs needed

Extend (via supersession) the `compute_participation_policy` contribution with three new fields:

- `market_dispatch_threshold_queue_depth` — max acceptable queue position from `/match` (default: 10)
- `market_dispatch_max_wait_ms` — abandon market dispatch and fall back to local if push + fallback-poll don't terminate within this budget (default: 60s for interactive builds, 600s for pyramid L0 batches)
- `market_dispatch_eager` — when true, try market first for every call; when false, try market only when local would be serialized (default: false initially, operators opt-in)

All three have `#[serde(default)]` — legacy YAMLs without them continue to deserialize.

---

## 3. Module-by-module implementation plan

### 3.1 New module: `src-tauri/src/pyramid/compute_requester.rs`

**Responsibility:** 3-step HTTP client for the match→fill flow + waiter map for push consumption + fallback poll on timeout.

```rust
/// Opaque request handle returned from match; held by the waiter.
pub struct MarketRequestHandle {
    pub job_id: String,          // handle-path
    pub request_id: String,
    pub matched_rate_in:  i64,
    pub matched_rate_out: i64,
    pub estimated_deposit: i64,
    pub queue_position: u32,
    // Internal: oneshot receiver for the inbound push.
    result_rx: oneshot::Receiver<Result<MarketResult, DeliveryError>>,
}

pub struct MarketResult {
    pub content: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model_used: String,
    pub latency_ms: i64,
    pub finish_reason: Option<String>,
}

pub enum DeliveryError {
    ProviderFailed { code: String, message: String },
    DeliveryTimedOut { waited_ms: u64 },
    DeliveryTombstoned, // Wire exhausted push attempts; poll confirmed `delivery_status: "failed"`.
}

/// Steps 1+2 combined. Returns a handle whose `result_rx` wakes when
/// Wire's delivery worker pushes to `/v1/compute/job-result`.
///
/// Side effect: registers a oneshot sender in the global `PendingJobs`
/// map keyed by job_id. Inbound handler (§3.2) looks up and fires.
pub async fn dispatch_market(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    pending_jobs: &PendingJobs,
    req: MarketInferenceRequest,
) -> Result<MarketRequestHandle, RequesterError>;

/// Await the oneshot with timeout + fallback poll on exhaustion.
///
/// Happy path: oneshot fires (push arrived) → Ok(MarketResult).
/// Timeout reached before push: issue ONE GET /jobs/:job_id to check
/// delivery_status. If "failed" → DeliveryTombstoned. If still
/// "executing" or "delivering" → DeliveryTimedOut (caller falls back).
pub async fn await_result(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    handle: MarketRequestHandle,
    max_wait_ms: u64,
) -> Result<MarketResult, RequesterError>;

pub async fn call_market(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    pending_jobs: &PendingJobs,
    req: MarketInferenceRequest,
    max_wait_ms: u64,
) -> Result<MarketResult, RequesterError>;

pub struct MarketInferenceRequest {
    pub model_id: String,
    pub messages: serde_json::Value,
    pub temperature: Option<f32>,
    pub max_tokens: usize,        // required per DD-W28
    pub input_token_count: i64,
    pub max_budget: i64,
    pub latency_preference: LatencyPreference, // best_price | balanced | lowest_latency
    pub privacy_tier: String,      // default "bootstrap-relay"
}

pub enum RequesterError {
    NoMatch { retry_after_secs: Option<u64> },
    InsufficientBalance { need: i64, have: i64 },
    MatchFailed { status: u16, body: String },
    FillFailed { status: u16, body: String, reason: Option<String> },
    Delivery(DeliveryError),
    Internal(String),
}
```

### 3.2 New inbound route: `POST /v1/compute/job-result`

Lives in `src-tauri/src/pyramid/routes_operator.rs` (sibling of existing `/v1/compute/job-dispatch`). Handles Wire's delivery worker push.

**Auth:** `Authorization: Bearer <jwt>` where JWT claims per contract §2.5:
- `aud == "result-delivery"`
- `iss == "wire"`
- `sub == <uuid_job_id>` matching body.job_id
- `rid == <requester_operator_id>` matching this node's operator_id
- `exp` not expired
- Ed25519 signature valid against Wire's shared public key

New node-side helper `verify_result_delivery_token` — sibling of `verify_market_identity`, same public-key read path, different audience constant.

**Body (success):**
```json
{"type": "success", "job_id": "<uuid>", "result": {"content": "...", "input_tokens": N, "output_tokens": N, "model_used": "...", "latency_ms": N, "finish_reason": "..."}}
```

**Body (failure):**
```json
{"type": "failure", "job_id": "<uuid>", "error": {"code": "...", "message": "..."}}
```

**Handler flow:**
1. Verify JWT via `verify_result_delivery_token`. 401 on fail.
2. Parse body (`deny_unknown_fields` + `extensions` escape hatch).
3. Look up `pending_jobs.remove(&body.job_id)`. If absent (node restart, duplicate delivery after timeout-fallback-to-local) → 2xx with `{"status": "already_settled"}` so Wire marks delivery done and doesn't retry. Emit chronicle `compute_delivery_late_arrival`.
4. Fire the oneshot sender with the parsed result (success or failure variant).
5. 2xx response.

Idempotency: matches contract §2.5 — Wire may retry; node returns 2xx + `already_settled` for duplicates.

### 3.3 Pending-jobs map

```rust
pub struct PendingJobs {
    inner: Arc<RwLock<HashMap<String /* uuid job_id */, oneshot::Sender<DeliveryPayload>>>>,
}
```

Registered on AppState + cloned into OperatorContext at boot. `dispatch_market` inserts; inbound handler removes + fires. If `await_result` times out, it explicitly removes its own entry to clean up (preventing a late push from trying to fire a dropped channel).

### 3.4 Integration into `call_model_unified`

New cascade branch added to `src-tauri/src/pyramid/llm.rs::call_model_unified`:

```rust
if should_try_market(&policy, &balance, &tier) {
    match compute_requester::call_market(&auth, &config, &pending_jobs, req, policy.market_dispatch_max_wait_ms).await {
        Ok(market_result) => return Ok(LlmResponse::from_market(market_result)),
        Err(RequesterError::NoMatch { .. })
        | Err(RequesterError::Delivery(DeliveryError::DeliveryTimedOut { .. }))
        | Err(RequesterError::Delivery(DeliveryError::DeliveryTombstoned)) => {
            tracing::info!("market dispatch unavailable; falling back to local");
            // fall through
        }
        Err(RequesterError::InsufficientBalance { .. })
        | Err(RequesterError::FillFailed { status: 401, .. }) => {
            return Err(format_err(e));  // auth + balance errors bubble up
        }
        Err(e) => return Err(format_err(e)),
    }
}
// ... existing provider dispatch ...
```

`should_try_market`: policy `allow_market_dispatch` + balance ≥ estimated deposit + tier eligibility.

### 3.5 Policy extensions

Three new fields on `ComputeParticipationPolicy` in `local_mode.rs`:

```rust
#[serde(default = "default_market_dispatch_threshold_queue_depth")]
pub market_dispatch_threshold_queue_depth: u32,  // 10

#[serde(default = "default_market_dispatch_max_wait_ms")]
pub market_dispatch_max_wait_ms: u64,  // 60_000

#[serde(default)]
pub market_dispatch_eager: bool,  // false
```

### 3.6 Chronicle events

Emit four new event types via the existing chronicle mechanism:
- `EVENT_COMPUTE_MARKET_DISPATCHED` — `/fill` returned 2xx. Metadata: `{job_id, queue_position, matched_rate_in/out}`.
- `EVENT_COMPUTE_MARKET_DELIVERED` — inbound push succeeded. Metadata: `{job_id, input_tokens, output_tokens, latency_ms, model_used}`.
- `EVENT_COMPUTE_MARKET_FELL_BACK_LOCAL` — market unavailable and fell back to local. Metadata: `{reason}`.
- `EVENT_COMPUTE_MARKET_DELIVERY_LATE_ARRIVAL` — push arrived after pending_jobs entry was already removed (timeout + fallback). Metadata: `{job_id, time_since_dispatch_ms}`.

Plus a synthetic per-build summary:
- `EVENT_BUILD_MARKET_USAGE` — emitted at `pyramid_build` completion. Metadata: `{total_llm_calls, market_calls, local_calls, openrouter_calls, avg_market_latency_ms, total_credits_spent}`. Powers the Builds-tab capability moment (UI lives in the other thread).

---

## 4. Auth flows

### 4.1 Node → Wire (`/match`, `/fill`, `/jobs/:job_id`)

Bearer token from `AuthState.api_token` (the `gne_live_*` machine token). No new auth mechanism — same path as `/offers`.

### 4.2 Wire → Node (inbound push on `/v1/compute/job-result`)

Wire-minted EdDSA JWT with claim shape:

```
aud: "result-delivery"
iss: "wire"
sub: <uuid_job_id>
rid: <requester_operator_id>
exp: <now + delivery_attempt_ttl_secs (120s per Wire-side lock)>
iat: <now>
```

Same signing key as `wire_job_token`. Node verifies via `verify_result_delivery_token` — sibling of `verify_market_identity` with different audience check.

Why JWT (not symmetric bearer): no secrets-at-rest on Wire, bound delivery to specific job via `sub` + specific requester via `rid`, same key material as dispatch JWT so infrastructure is already trusted on both sides.

---

## 5. Error handling matrix — requester side

| From Wire | Node behavior |
|---|---|
| 200 match + 2xx fill | Happy path. Await oneshot with timeout. |
| 402 match (insufficient balance) | Hard failure. Surface to operator: "top up or enable local fallback." No silent fallback — masks billing. |
| 404 match (no provider for model) | Fall back to local if policy allows; else surface. |
| 503 fill after rematch (queue cap or foreign dispatcher) | Fall back to local. Market can't absorb right now. |
| 503 fill market_serving_disabled / compute_held | Fall back with Retry-After; Wire already handled offer-state. |
| 401 anywhere | Hard failure. Operator session broken. No silent fallback. |
| Push arrived successfully | Oneshot fires → Ok(MarketResult). |
| Push timeout (max_wait_ms) + poll shows `delivery_status: "failed"` | Fall back to local. Emit `compute_market_delivery_tombstoned` chronicle. |
| Push timeout + poll shows still-executing | Fall back to local. Emit `compute_market_delivery_timed_out`. Wire may still deliver later; inbound handler will log `late_arrival` and ACK with `already_settled`. |
| Push arrives as `{type: "failure"}` | Surface `DeliveryError::ProviderFailed` → `pyramid_build` retries the step. |

**Silent-fallback vs hard-error line:** market capacity issues (no match, full queue, provider fault, delivery timeout) fall back silently — market is additive. Auth / balance violations bubble up — they indicate operator-facing problems.

---

## 6. What stays unchanged (Phase 2 provider side)

- **`handle_market_dispatch` + `spawn_market_worker`** (inbound job dispatch): zero changes.
- **Offer management** (`compute_market_ops` create/update/remove): zero changes.
- **Queue mirror push** (`market_mirror`): zero changes.
- **Operator HTTP surface** (existing 25 routes in `routes_operator.rs`): zero changes; one NEW route added (`POST /v1/compute/job-result`).
- **`verify_market_identity`**: zero changes. New sibling `verify_result_delivery_token` added alongside, same public-key plumbing.

---

## 7. Scope boundaries

### 7.1 In this phase
- `compute_requester.rs` module (match→fill client, pending_jobs, await-on-oneshot-with-timeout-fallback)
- `verify_result_delivery_token` sibling helper (JWT `aud=result-delivery`)
- New `POST /v1/compute/job-result` route in `routes_operator.rs`
- `call_model_unified` integration with market-dispatch branch + fallback
- Policy extensions (3 new fields on `ComputeParticipationPolicy`)
- Chronicle events (4 new + 1 synthetic build summary)
- CLI: `compute-market-call <model-id> --prompt "..."` + `compute-market-jobs [--limit N]` for debug + observability

### 7.2 Deferred
- **Builds-tab capability moment** — handled by invisibility UX thread. Phase 3 populates the chronicle data; rendering is separate concern.
- **Persistence of pending_jobs map across node restart** — Phase 3 accepts loss on restart; `pyramid_build` retries the step. Future phase if restart-loss becomes a real tester pain point.
- **Pooled/batched dispatch** (1 match call for N inference calls) — post-baseline optimization.
- **Per-tier market configuration** (market primary only, market fallback only) — future refinement.
- **Relay market support** — separate Wire-side workstream.

### 7.3 Explicit non-goals
- **No symmetric bearer-token auth on inbound push** — Option Y is locked; JWT-only.
- **No content retrieval via `GET /jobs/:job_id`** — by contract design, poll is status-only.
- **No changes to provider-side dispatch surface**.

---

## 8. Testing strategy

### 8.1 Unit tests
- `compute_requester` error classification (mock `send_api_request`; verify each error variant maps correctly)
- `verify_result_delivery_token` — valid JWT, wrong aud, wrong rid, expired, wrong signature, missing claims (per claim shape §4.2)
- Pending-jobs map: register, fire, timeout cleanup, duplicate-inbound-after-removal idempotency
- Policy gate: `should_try_market` combinatorics across policy × balance × tier

### 8.2 Integration smoke (once Wire W3+W4 on dev)
- **Happy path:** `pyramid-cli compute-market-call gemma4:26b --prompt "hi"` → match → fill → push arrives → content returned. Chronicle shows dispatched + delivered.
- **Fallback on no match:** disable all matching offers on dev → market returns 404 → falls back to local → build completes.
- **Timeout + poll detects tombstone:** simulate provider that 202-ACKs but never completes → node's max_wait_ms expires → poll returns `delivery_status: "failed"` → falls back.
- **Late arrival after timeout:** same as above but Wire delivers after node moved on → inbound handler ACKs with `already_settled` + emits `late_arrival` chronicle.
- **Multi-call build:** small pyramid build (~20 L0 calls) with market enabled → chronicle shows mixed market + local distribution.

### 8.3 Regression
All 1600+ existing library tests remain green. Provider-side compute-market tests unchanged.

---

## 9. Delivery sequence within Phase 3

Realistic, corrected-pessimistic estimates:

1. **3a — JWT verify helper** (~15 min)
   `verify_result_delivery_token` + unit tests. Independent of any live Wire endpoint; can start immediately.
2. **3b — Policy extensions** (~15 min)
   Three fields on `ComputeParticipationPolicy`.
3. **3c — Pending-jobs map + inbound route** (~30 min)
   `PendingJobs` struct, oneshot plumbing, `POST /v1/compute/job-result` handler registered alongside existing operator routes.
4. **3d — `compute_requester.rs` client** (~45 min)
   `dispatch_market`, `await_result`, error taxonomy. Unit tests with mocked HTTP.
5. **3e — `call_model_unified` integration** (~30 min)
   Market branch + fallback gate + `should_try_market`.
6. **3f — Chronicle events** (~15 min)
   4 new event types + build summary.
7. **3g — CLI commands** (~15 min)
   `compute-market-call` + `compute-market-jobs` in `mcp-server/src/cli.ts`.
8. **3h — Smoke against Wire W3+W4 dev** (~30–60 min)
   Full matrix per §8.2.
9. **3i — Commit + push**.

**Total: ~3–4h realistic. Can ship one session once W3+W4 hit dev.**

Steps 3a + 3b are independent of Wire being live — could pre-stage tonight if I hadn't already decided to hold. Decided to hold anyway because my "I'll just pre-stage a little" instinct has compounded wrong assumptions twice this session. Will start fresh against live Wire tomorrow.

---

## 10. Success criteria

Tester runs `pyramid-cli question-build opt-025 "what is X?"` on a node with `market_dispatch_eager=true` and a sane balance.

Build completes. Chronicle shows majority of L0 calls went to market providers. Build time is dramatically faster than local-only.

Non-technical friend, given 30 seconds of UI, says "oh, the pyramid built really fast because of the network." That's the pass criterion.

---

## 11. Revision log

| Rev | Date | Change |
|---|---|---|
| 0.1 | 2026-04-17 | Initial plan. Framed Phase 3 as "polling-only, push deferred to 3.5." |
| 0.2 | 2026-04-17 | Small amendment — jobs-poll endpoint resolved as W4 scope; added poll-vs-push tradeoff table. Still polling-primary framing. |
| 0.3 | 2026-04-17 late-night | **Major rewrite.** After live bilateral with Wire owner, polling-only framing was recognized as impossible — content is push-only. Push promoted to Phase 3 core. New inbound route `POST /v1/compute/job-result` added. Auth via Wire-minted JWT with `aud=result-delivery` (Option Y — no secrets-at-rest, symmetric with existing dispatch JWT infrastructure). §1.1 rewritten to four-beat flow. §3 restructured: new inbound route module, pending-jobs map, JWT verify sibling. §7 updated to reflect push is in-scope. Policy knobs unchanged. Target contract reference moves from rev 1.2 → rev 1.4 (with new §2.5 delivery path). |
