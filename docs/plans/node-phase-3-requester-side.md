# Node Phase 3 — Requester-Side Compute Market

**Status:** Plan. Ready to start implementation the moment Wire ships W3/W4 on dev.
**Scope:** Node-side only. Teaches `pyramid_build` (and its siblings) to dispatch inference through the compute market instead of always hitting local/OpenRouter.
**Dependencies:** Wire W3 (`/match`, `/fill`, wire_job_token JWT mint) + W4 (`/callback/:job_id` handler + `wire_compute_result_transit` TTL + `GET /jobs/:job_id` poll endpoint).
**Contract reference:** `GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md` rev 1.2.
**Estimated scope:** ~6–8h originally estimated; corrected-pessimistic heuristic says 1–3h realistic.

---

## 1. What Phase 3 actually is

Phase 2 shipped the **provider side**: this node receives dispatches from Wire, runs inference, POSTs results back.

Phase 3 ships the **requester side**: this node, when building a pyramid, **asks the market for inference instead of (or in addition to) using local Ollama / OpenRouter**.

The payoff: a tester's 200-node L0 cluster build that takes 75 minutes locally takes ~45 seconds on the network, because 184 other nodes run 184 of the 200 inference calls concurrently.

### 1.1 What the requester side looks like

From the node's perspective, each inference call that would normally go to `call_model_unified` now has a new option: **dispatch via market**. That's a three-step HTTP sequence:

1. `POST /api/v1/compute/match` — ask Wire to find a provider. Returns `{job_id, matched_rate_in/out, matched_multiplier_bps, reservation_fee, estimated_deposit, queue_position, request_id}`. Wire has already debited a reservation from this operator's balance.
2. `POST /api/v1/compute/fill` with `Idempotency-Key: <request_id>` — Wire mints a `wire_job_token` JWT (aud=compute), generates an opaque callback-auth bearer, and POSTs `MarketDispatchRequest` to the provider's tunnel. Returns immediately (2xx) once the provider ACKs.
3. `GET /api/v1/compute/jobs/:job_id` — poll until status is terminal (`completed` / `failed` / `timed_out`). On `completed`, the result content is in the transit table (1-hour TTL) and available in the poll response.

Step 3 is a polling loop because the node is the REQUESTER, not the callback target — **Wire hosts the callback, not the requester node**. The provider POSTs its result to Wire's `/api/v1/compute/callback/:job_id`; Wire stores it in `wire_compute_result_transit`; the requester polls Wire for it.

**This is important:** node-side Phase 3 adds ZERO new inbound HTTP routes. The only new HTTP surface is client-side (node hitting Wire). The provider-side dispatch handler from Phase 2 is unchanged.

---

## 2. Where it slots into the node's existing inference paths

### 2.1 The inference fan-out lives at `call_model_unified`

Every LLM call in the build pipeline eventually routes through `call_model_unified` (in `src-tauri/src/pyramid/llm.rs`). It already has tier dispatch (primary / fallback_1 / fallback_2) + provider dispatch (openrouter / ollama-local / bridge-tier / dispatched-to-fleet). Adding a **market dispatch branch** is the clean extension point.

### 2.2 The decision: when does a call go to market?

Per `compute_participation_policy`:
- `allow_market_dispatch = true` AND
- current balance ≥ estimated deposit AND
- `/match` returns a provider in reasonable queue position (configurable threshold)

If any of those fail, fall through to existing provider dispatch (local Ollama / OpenRouter). The market is an **additive** path, not a replacement.

### 2.3 Policy knobs needed

Extend (via supersession) the `compute_participation_policy` contribution with:
- `market_dispatch_threshold_queue_depth` — max acceptable queue position from `/match` (default: 10)
- `market_dispatch_max_wait_ms` — abandon market dispatch and fall back to local if poll doesn't terminate within this budget (default: 60s for interactive builds, 600s for pyramid L0 batches)
- `market_dispatch_eager` — when true, try market first for every call; when false, try market only for batch/concurrent-heavy phases (default: false initially, operators opt-in)

All three are new supersedable fields on the existing policy contribution. Node-side `ComputeParticipationPolicy` struct grows three optional fields with sensible defaults.

---

## 3. Module-by-module implementation plan

### 3.1 New client module: `src-tauri/src/pyramid/compute_requester.rs`

**Responsibility:** the 3-step HTTP client for the market dispatch flow. Pure client — no server, no state.

```rust
pub struct MarketRequestHandle {
    pub job_id: String,          // handle-path
    pub request_id: String,
    pub matched_rate_in:  i64,   // for observability only
    pub matched_rate_out: i64,
    pub estimated_deposit: i64,
    pub queue_position: u32,
}

pub struct MarketResult {
    pub content: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model_used: String,
    pub latency_ms: i64,
    pub finish_reason: String,
}

/// Step 1 + 2 combined. Matches, then immediately fills. Returns the
/// handle you'll poll on. Uses Idempotency-Key on /fill so retries
/// are safe.
pub async fn dispatch_market(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    req: MarketInferenceRequest,
) -> Result<MarketRequestHandle, ComputeRequesterError>;

/// Step 3. Poll until terminal. Internal exponential-backoff: 500ms,
/// 1s, 2s, 4s, 8s, plateau at 8s. Abandons after `max_wait_ms`.
pub async fn await_result(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    handle: &MarketRequestHandle,
    max_wait_ms: u64,
) -> Result<MarketResult, ComputeRequesterError>;

/// Convenience: dispatch + await in a single call.
pub async fn call_market(
    auth: &Arc<RwLock<AuthState>>,
    config: &Arc<RwLock<WireNodeConfig>>,
    req: MarketInferenceRequest,
    max_wait_ms: u64,
) -> Result<MarketResult, ComputeRequesterError>;

pub struct MarketInferenceRequest {
    pub model_id: String,
    pub messages: serde_json::Value,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub privacy_tier: String,           // default "bootstrap-relay"
}

pub enum ComputeRequesterError {
    NoMatch { retry_after_secs: Option<u64> },   // 503, no provider available right now
    InsufficientBalance { need: i64, have: i64 }, // 402 from match
    MatchFailed { status: u16, body: String },
    FillFailed { status: u16, body: String, reason: Option<String> },  // reason = X-Wire-Reason
    Timeout { waited_ms: u64 },
    JobFailed { code: String, message: String }, // provider returned type="failure"
    Internal(String),
}
```

Dispatch flow:

```
match   → request_id, job_id, rates, queue_pos, deposit
           ↓
fill    (idempotency key = request_id)
           → 2xx ACK → handle
           → 503 X-Wire-Reason=queue_depth_exceeded → Wire rematches once automatically
              → still 503 after rematch → NoMatch error
           → other 503 variants per contract §2.2 → per-reason error
           ↓
poll    GET /jobs/:job_id (500ms, 1s, 2s, 4s, 8s+)
           → "queued" / "executing" → keep polling
           → "completed" → extract result → Success
           → "failed" / "timed_out" → JobFailed
           → polling exceeds max_wait → Timeout
```

### 3.2 Integration point: `call_model_unified`

Add a new cascade branch in `src-tauri/src/pyramid/llm.rs::call_model_unified`:

```rust
async fn call_model_unified(...) -> Result<LlmResponse> {
    // ... existing code ...

    // NEW: market-dispatch attempt before falling through to local/bridge
    if should_try_market(&policy, &balance, &tier) {
        match compute_requester::call_market(
            &auth, &config,
            MarketInferenceRequest::from_current_request(...),
            policy.market_dispatch_max_wait_ms,
        ).await {
            Ok(market_result) => return Ok(LlmResponse::from_market(market_result)),
            Err(ComputeRequesterError::NoMatch { .. }) |
            Err(ComputeRequesterError::Timeout { .. }) => {
                tracing::info!("market dispatch unavailable; falling back to local");
                // fall through to existing path
            }
            Err(e) => {
                // Hard failures (401, insufficient balance, job-failed) should not
                // silently fall back — that'd hide billing issues from the operator.
                return Err(format_err(e));
            }
        }
    }

    // ... existing provider dispatch ...
}
```

`should_try_market` gate: policy flag + balance check + tier eligibility (some tiers may not be market-eligible; e.g., if primary tier is local Ollama and the user is on a desktop with a good GPU, there's no need for market).

### 3.3 Policy extensions

Add three fields to `ComputeParticipationPolicy` in `src-tauri/src/pyramid/local_mode.rs`:

```rust
pub struct ComputeParticipationPolicy {
    // ... existing fields ...

    #[serde(default = "default_market_dispatch_threshold_queue_depth")]
    pub market_dispatch_threshold_queue_depth: u32,  // 10

    #[serde(default = "default_market_dispatch_max_wait_ms")]
    pub market_dispatch_max_wait_ms: u64,  // 60_000 (60s)

    #[serde(default)]
    pub market_dispatch_eager: bool,  // false
}
```

All three have `#[serde(default)]` — legacy YAMLs without them continue to deserialize.

### 3.4 Observability: chronicle events

Emit four new event types via the existing chronicle mechanism:
- `EVENT_MARKET_DISPATCHED` — when `/fill` returns 2xx. Metadata: `{job_id, queue_position, matched_rate_in, matched_rate_out}`.
- `EVENT_MARKET_MATCH_FAILED` — when `/match` returns non-2xx. Metadata: `{status, reason}`.
- `EVENT_MARKET_FELL_BACK_LOCAL` — when market dispatch couldn't match AND local fallback kicked in. Metadata: `{reason}`.
- `EVENT_MARKET_COMPLETED` — when result arrives back. Metadata: `{job_id, input_tokens, output_tokens, latency_ms, model_used}`.

Plus a synthetic "build summary" event at `pyramid_build` end:
- `EVENT_BUILD_MARKET_USAGE` — aggregate stats for the build. Metadata: `{total_llm_calls, market_calls, local_calls, openrouter_calls, avg_market_latency_ms, total_credits_spent}`. Powers the Builds-tab capability moment.

### 3.5 Builds-tab capability moment (frontend — deferred to UX-thread fork)

The Builds tab needs to surface:

> Built in 47s using 184 network GPUs
> (Solo build would have taken ~14m)

Data available via the chronicle aggregation above. This is UI work handled by the **other thread** doing the invisibility UX pass. My Phase 3 just ensures the chronicle data is populated; rendering is their concern.

---

## 4. Auth flow

`/match` and `/fill` + `/jobs/:job_id` are all authenticated with the **same** Bearer token the node already uses for `/offers` — the `gne_live_...` machine token from `AuthState.api_token`. No new auth mechanism. Same `get_api_token(&auth)` call path.

Wire verifies operator identity + balance via the bearer; no JWT signing on the requester side.

---

## 5. Error handling matrix — requester side

| From Wire | Node behavior |
|---|---|
| 200 match + 2xx fill | Happy path, start polling |
| 402 match (insufficient balance) | Surface to operator with "top up or enable local fallback" |
| 503 match (no provider for model) | Fall back to local if policy allows; else surface |
| 503 fill queue_depth_exceeded (after Wire's 1 rematch) | Fall back to local (market can't absorb right now) |
| 503 fill market_serving_disabled | Fall back; Wire already deactivated the offer |
| 503 fill market_compute_held | Fall back with Retry-After hint; Wire will match us elsewhere next time |
| 401 anywhere | Hard failure; operator session is broken, don't silently fall back (would mask the bug) |
| Timeout during poll | Hard failure bubbled up; operator sees "market call exceeded wait budget" |
| 402 poll (transit TTL expired, result gone) | Same as a job failure — try the build step again |
| Any unexpected 5xx | Fall back to local once; second failure = hard error |

**The silent-fallback vs hard-error line:** market capacity issues (no match, full queue) fall back silently because the market is additive. Auth / balance / policy violations bubble up because they indicate operator-facing problems.

---

## 6. What stays the same

- **Provider-side dispatch handler** (`handle_market_dispatch` + `spawn_market_worker`): zero changes. This handles inbound jobs FROM Wire; Phase 3 doesn't touch it.
- **Offer management** (`compute_market_ops::create_offer` etc): zero changes. Offers are the provider-side surface.
- **Queue mirror push** (`market_mirror`): zero changes. Provider-side.
- **HTTP operator surface** (`routes_operator.rs` 25 routes): zero changes. Agent / CLI surface is stable.
- **JWT verification** (`market_identity::verify_market_identity`): zero changes. Only the provider path calls this.

---

## 7. Scope boundaries

### 7.1 In this phase
- `compute_requester.rs` module (client, 3-step flow, poll loop, error taxonomy)
- `call_model_unified` integration with market-dispatch branch + fallback
- Policy extensions (3 new fields on `ComputeParticipationPolicy`)
- Chronicle events (4 new + 1 synthetic build summary)
- CLI commands on `pyramid-cli` for observability:
  - `compute-market-call <model-id> --prompt "..."` — dispatch a one-off market call for debugging (no build context)
  - `compute-market-jobs [--limit N]` — list recent requester-side jobs with outcomes

### 7.2 Deferred
- Builds-tab capability moment — handled by invisibility UX thread.
- Market-only mode (market OR fail, no local fallback) — policy knob for future.
- Pooled/batched dispatch (1 match call for 20 inference calls) — optimization after baseline works.
- Per-tier market configuration (market primary only, market fallback only, etc.) — future refinement.
- Relay market support — separate Wire-side workstream + separate node-side phase.

### 7.3 Explicit non-goals
- No new inbound HTTP routes on the node. Requester-side is pure client.
- No new JWT signing on the node. Requester-side uses operator Bearer only.
- No changes to the provider-side dispatch surface.

---

## 8. Testing strategy

### 8.1 Unit tests
- `compute_requester` error classification (mock `send_api_request` responses; verify each error variant maps correctly)
- Poll-loop backoff schedule (verify exponential backoff → plateau at 8s)
- Policy gate logic (`should_try_market` combinatorics across policy × balance × tier)

### 8.2 Integration smoke (once Wire W3+W4 on dev)
- Happy path: `pyramid-cli compute-market-call gemma4:26b --prompt "hi"` → round-trip green, result printed
- Fallback: artificially starve the market (all provider offers disabled) → build completes via local
- Timeout: set `market_dispatch_max_wait_ms=2000` on a slow-provider call → timeout surfaces correctly
- Multi-call build: run a small pyramid build (~20 L0 calls) with market enabled → chronicle shows mixed market + local

### 8.3 Regression
- Existing provider-side tests unchanged; all 1595+ library tests must remain green

---

## 9. Sequencing handoff from Wire

Per Wire's latest message — W3 rough shape:

> POST /api/v1/compute/match — calls match_compute_job RPC (already live from W0), strips provider identity, returns {job_id, matched_rate_in/out, matched_multiplier_bps, reservation_fee, estimated_deposit, queue_position, request_id}
>
> POST /api/v1/compute/fill with Idempotency-Key header — mints wire_job_token JWT, generates opaque callback-auth bearer, POSTs MarketDispatchRequest to provider at {tunnel}/v1/compute/job-dispatch, handles all 7 X-Wire-Reason 503 variants, rematches once on queue_depth_exceeded, refunds reservation on provider-fault reasons.
>
> POST /api/v1/compute/callback/:job_id — provider POSTs result envelope with opaque bearer; verifies via sha256 lookup; idempotent on repeat; settles via settle_compute_job or fail_compute_job.

And crucially for me: a `GET /api/v1/compute/jobs/:job_id` poll endpoint on Wire — I need this for step 3 of my flow. **If it's not in Wire's W3+W4 scope, I need to raise it before implementation.** Flagging for Wire owner.

### 9.1 Open question for Wire

> The requester-side poll endpoint `GET /api/v1/compute/jobs/:job_id` returning `{status, result?, error?}` — is this in W3/W4, or a W5 concern? Contract §2.4 references it for polling during transit TTL, but Wire's W3/W4 summary doesn't list it explicitly. If it's not in W3/W4, I'll need it before node Phase 3 can smoke end-to-end.

---

## 10. Delivery sequence within Phase 3

1. **Phase 3a — client module** (~30 min)
   `compute_requester.rs` with all three functions + error taxonomy. Unit tests with mocked HTTP.
2. **Phase 3b — policy extensions** (~15 min)
   Three fields on `ComputeParticipationPolicy`. Migration: `#[serde(default)]` on all. No data migration needed.
3. **Phase 3c — `call_model_unified` integration** (~45 min)
   Add market branch + fallback logic + should_try_market gate.
4. **Phase 3d — chronicle events** (~20 min)
   Four new event types + build summary event.
5. **Phase 3e — CLI commands** (~15 min)
   `compute-market-call` + `compute-market-jobs` in `mcp-server/src/cli.ts`.
6. **Phase 3f — smoke against Wire W3/W4 dev** (~30 min)
   Full happy path + fallback + timeout end-to-end.
7. **Phase 3g — commit + push**

Total: ~3h realistic (vs 6–8h pessimistic). Can ship in one session once Wire W3+W4 are available on dev.

---

## 11. What Phase 3 doesn't yet unblock

- **GPU-less tester experience.** Testers with no local GPU need credits from somewhere. Phase 3 lets them spend credits but doesn't give them an easy path to earn them. Earning paths exist (intelligence contribution, hosting) but aren't integrated into the tester onboarding. Flag for alpha roadmap.
- **The capability moment.** Phase 3 populates the chronicle data; the Builds-tab UI that renders "built in 47s using 184 network GPUs" is invisibility-UX-thread work. Phase 3 can ship without it; the capability moment is what makes Phase 3 *feel* magical but Phase 3 is *correct* without it.

---

## 12. Success criteria

Tester runs `pyramid-cli question-build opt-025 "what is X?"` on a node with `market_dispatch_eager=true`.

Build completes. Chronicle shows the majority of L0 calls went to market providers. Build completes in seconds instead of minutes.

If I showed this to the same non-technical friend from the invisibility UX success criterion and asked "what happened?" — they'd say "oh, the pyramid built really fast because of the network." That's the pass criterion.
