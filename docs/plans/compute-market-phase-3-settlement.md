# Phase 3: Settlement & Requester Integration

**What ships:** Full credit loop. Requester's chain executor dispatches to Wire compute. Settlement works. Result delivery via webhook. DADBEAR preview gate estimates market costs before committing.

**Prerequisites:** Phase 2 (exchange, provider-side queue), ACK+async result delivery pattern (resolves Cloudflare 524 timeout -- this is a PREREQUISITE, not optional)

---

## I. Overview

Phase 3 closes the economic loop. Phase 2 left providers able to publish offers and requesters able to match jobs on the exchange. Phase 3 makes the money work: settlement pays providers, refunds requesters, levies the Graph Fund, and records performance observations. On the node side, Phase 3 adds `WireComputeProvider` as a new provider type in the LLM dispatch path so pyramid builds can consume market compute. DADBEAR work items wrap every outbound market call with preview gates, cost estimation, and crash recovery. The provider side reports settlement metadata and records chronicle events for completed market jobs.

The critical architectural constraint: **the Wire never sees prompts or results**. For launch (0-relay, standard privacy tier), the Wire proxies the requester's prompt to the provider's tunnel URL. This means the Wire does see the payload in transit for standard-tier jobs. This is explicitly acknowledged and matches the privacy model of existing cloud providers (OpenRouter). Higher privacy tiers (relay chains, Clean Room) are stubbed but not built in Phase 3.

---

## II. Wire Workstream

### RPCs Built This Phase

All RPCs follow the atomic credit-engine pattern: call `credit_operator_atomic`/`debit_operator_atomic` for all credit movements. No raw writes to `wire_operators` or `wire_credits_ledger`. All RPCs are `SECURITY DEFINER` with corresponding `GRANT EXECUTE ... TO service_role`.

#### `settle_compute_job(p_job_id, p_prompt_tokens, p_completion_tokens, p_latency_ms, p_finish_reason)`

Canonical SQL is in the monolithic plan (lines 532-712) with the following bug fixes applied:

1. **model_id filter on queue decrement.** The `UPDATE wire_compute_queue_state` must include `AND model_id = v_job.model_id`. Without it, a node with multiple model queues decrements the wrong queue row. Same fix applies to `wire_compute_offers` decrement (keyed by `offer_id`, already correct, but the queue state row needs the model filter).

2. **Single operator resolution.** The plan resolves `v_wire_platform_operator_id` twice -- once at the top of the function and again inside the `v_requester_adj < 0` branch. The duplicate resolution is removed. The single resolution at the top of the function is sufficient.

3. **Completion token guard.** Caps `p_completion_tokens` at 2x `v_job.max_tokens` to prevent absurd settlement reports from inflating provider payouts.

Settlement flow:
- Lock job row (`status = 'executing'`, `FOR UPDATE`)
- Calculate actual cost from measured tokens (integer arithmetic, Pillar 9)
- Advance rotator arm: `advance_market_rotator(node_id, 'compute', model_id, 'settlement')`
- Route payment to rotator-determined recipient (provider 76/80, Wire 2/80, Graph Fund 2/80)
- Deposit reconciliation: if actual < deposit, refund overage to requester. If actual > deposit, Wire platform operator absorbs the difference (may go negative, replenished from platform revenue). Requester never pays more than the deposit.
- Update job record to `status = 'completed'`
- Pay recipient via `credit_operator_atomic`
- Graph Fund insertion if on GF slot
- Record observation
- Decrement queue depth (with `AND model_id = v_job.model_id`)

#### `fail_compute_job(p_job_id, p_reason)`

- Lock job row (`status IN ('executing', 'filled')`)
- Refund deposit to requester via `credit_operator_atomic`
- Reservation fee stays with provider (non-refundable -- they held capacity)
- Update job to `status = 'failed'`
- Record failure observation (zero tokens, zero latency -- marks the failure in performance metrics)
- Decrement queue depth (with `AND model_id = v_job.model_id`)

#### `void_compute_job(p_job_id)`

- Lock job row (`status = 'reserved'`)
- No deposit was charged (unfilled slot)
- Reservation fee already with provider
- Update job to `status = 'void'`
- Decrement queue depth (with `AND model_id = v_job.model_id`)

#### `cancel_compute_job(p_job_id, p_requester_operator_id)`

**This RPC was missing from the plan (audit finding S11).** Added in Phase 3.

- Lock job row (`status IN ('reserved', 'filled')`, requester_operator_id matches)
- Cannot cancel jobs in `'executing'` status (GPU already working)
- If `status = 'filled'`: refund deposit to requester via `credit_operator_atomic`
- Reservation fee is non-refundable (provider held capacity)
- If `relay_count > 0` and relay fees were charged: refund relay fees (relays did no work)
- Update job to `status = 'cancelled'`
- Decrement queue depth (with `AND model_id = v_job.model_id`)

```sql
CREATE OR REPLACE FUNCTION cancel_compute_job(
  p_job_id UUID,
  p_requester_operator_id UUID
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
BEGIN
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id
      AND requester_operator_id = p_requester_operator_id
      AND status IN ('reserved', 'filled')
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found, not owned by requester, or not in cancellable status';
  END IF;

  -- Refund deposit if filled
  IF v_job.status = 'filled' AND COALESCE(v_job.deposit_amount, 0) > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_job.deposit_amount,
      'compute_cancel_refund', p_job_id, 'compute_market');
  END IF;

  -- Reservation fee stays with provider (non-refundable)

  -- Refund relay fees if any (relays did no work on cancellation)
  IF v_job.relay_count > 0 THEN
    -- Relay fees were escrowed to Wire platform operator.
    -- Reverse: credit requester, debit Wire platform.
    -- The relay settlement path (per-hop) never fires for cancelled jobs.
    NULL; -- Implementation: sum relay fees from ledger entries with category='relay_market'
          -- and reference_id=p_job_id, then credit_operator_atomic back to requester.
  END IF;

  UPDATE wire_compute_jobs SET
    status = 'cancelled',
    completed_at = now()
  WHERE id = p_job_id;

  -- Decrement queue depth
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id AND model_id = v_job.model_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;
END;
$$;

GRANT EXECUTE ON FUNCTION cancel_compute_job TO service_role;
```

#### `start_compute_job(p_job_id, p_provider_node_id)`

**This transition was undefined in the plan (audit finding: no `filled -> executing` transition).** All settlement RPCs check for `status = 'executing'` but nothing produced that status.

Called by the provider node when a filled job reaches the front of its GPU queue and begins execution. The Wire-side route handler verifies the provider identity.

```sql
CREATE OR REPLACE FUNCTION start_compute_job(
  p_job_id UUID,
  p_provider_node_id UUID
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
BEGIN
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id
      AND provider_node_id = p_provider_node_id
      AND status = 'filled'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found, wrong provider, or not in filled status';
  END IF;

  UPDATE wire_compute_jobs SET
    status = 'executing',
    started_at = now()
  WHERE id = p_job_id;
END;
$$;

GRANT EXECUTE ON FUNCTION start_compute_job TO service_role;
```

**Who triggers it, when:** The provider node calls `POST /compute/start` immediately before GPU execution begins. The GPU processing loop calls this after dequeuing a market job and before calling the LLM. This gives the Wire accurate timing data (started_at vs completed_at = actual GPU time, not queue wait time).

### API Routes Built This Phase

| Route | RPC | Auth |
|-------|-----|------|
| `POST /api/v1/compute/settle` | `settle_compute_job` | Provider node token + wire_job_token |
| `POST /api/v1/compute/void` | `void_compute_job` | Provider node token + wire_job_token |
| `POST /api/v1/compute/fail` | `fail_compute_job` | Provider node token + wire_job_token |
| `POST /api/v1/compute/cancel` | `cancel_compute_job` | Requester node token |
| `POST /api/v1/compute/start` | `start_compute_job` | Provider node token + wire_job_token |

All routes validate the caller's identity against the job record (provider routes check `provider_node_id`, requester routes check `requester_operator_id`).

### Performance Observation Recording

On every settlement, an observation row is inserted into `wire_compute_observations`:

```sql
INSERT INTO wire_compute_observations
  (job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec)
VALUES
  (p_job_id, v_job.provider_node_id, v_job.model_id,
   p_prompt_tokens, p_completion_tokens, p_latency_ms,
   CASE WHEN p_latency_ms > 0
     THEN p_completion_tokens::REAL / (p_latency_ms::REAL / 1000)
     ELSE 0
   END);
```

Failure observations record zero values (marks the failure event in the performance profile).

**Observation aggregation function** (needed before Phase 5 quality enforcement):

```sql
CREATE OR REPLACE FUNCTION aggregate_compute_observations(
  p_node_id UUID,
  p_model_id TEXT,
  p_horizon_hours INTEGER DEFAULT 168  -- 7 days
) RETURNS TABLE(
  median_latency_ms INTEGER,
  p95_latency_ms INTEGER,
  median_tps REAL,
  median_output_tokens INTEGER,
  observation_count INTEGER,
  failure_count INTEGER
)
LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN
  RETURN QUERY
  SELECT
    percentile_cont(0.5) WITHIN GROUP (ORDER BY o.latency_ms)::INTEGER,
    percentile_cont(0.95) WITHIN GROUP (ORDER BY o.latency_ms)::INTEGER,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY o.tokens_per_sec)::REAL,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY o.output_tokens)::INTEGER,
    COUNT(*)::INTEGER,
    COUNT(*) FILTER (WHERE o.latency_ms = 0)::INTEGER
  FROM wire_compute_observations o
  WHERE o.node_id = p_node_id
    AND o.model_id = p_model_id
    AND o.created_at > now() - (p_horizon_hours || ' hours')::interval;
END;
$$;

GRANT EXECUTE ON FUNCTION aggregate_compute_observations TO service_role;
```

This function is called by the heartbeat handler to populate the `performance_profile` in the heartbeat response, and by the market surface endpoint to provide speed rankings.

---

## III. Node Workstream

### WireComputeProvider -- CORRECTED

**The audit found a critical data flow contradiction (Theme 3a).** The original plan's `WireComputeProvider.fill_job()` code sketch sends `system_prompt, user_prompt` as parameters to the Wire. This is WRONG. The Wire never sees payloads.

**Correct flow for 0-relay (launch, standard privacy tier):**

1. Dispatch policy resolves a route to `wire-compute`
2. `WireComputeProvider.call()` is invoked from the LLM provider dispatch path in `llm.rs` (NOT from `chain_dispatch.rs`)
3. **Match:** `POST /api/v1/compute/match` with `model_id`, `max_budget`, `input_tokens`, `latency_preference`. NO prompts sent. Returns `job_id`, matched rates, estimated deposit, queue position.
4. **Fill:** `POST /api/v1/compute/fill` with `job_id`, `input_token_count` (computed locally via tiktoken), `relay_count=0`. NO PROMPTS SENT TO THE WIRE. Fill returns: `relay_chain` (empty for 0-relay), `provider_ephemeral_pubkey`, `total_relay_fee`, `deposit_charged`.
5. **For 0-relay (launch):** The Wire proxies the prompt to the provider's tunnel URL. The Wire dispatches the filled job to the provider via `POST {provider_tunnel_url}/v1/compute/job-dispatch` with the prompt included. **The Wire sees the payload in transit for standard-tier jobs.** This is explicitly acknowledged. It matches the privacy level of OpenRouter (cloud provider sees prompts). The provider's tunnel URL is never returned to the requester -- the Wire uses it internally.
6. **Provider ACKs immediately** (HTTP 202) to avoid Cloudflare 524 timeout. Provider queues the job, processes on GPU, reports settlement metadata to Wire.
7. **Wire forwards result** to requester via `POST {requester_tunnel_url}/v1/compute/result-delivery`. Requester resolves the oneshot channel. Chain executor task completes.

**For N-relay (future, not Phase 3):** Requester encrypts prompt with provider's ephemeral public key, sends through relay chain. Wire never sees plaintext payload. Relays stream ciphertext without reading it. Provider decrypts, executes, encrypts result, sends back through relay chain.

**WireComputeProvider does NOT reimplement fleet dispatch.** Fleet routing already happens in `llm.rs` Phase A BEFORE `wire-compute` is reached in the provider preference chain. `WireComputeProvider` receives calls only when fleet capacity is exhausted and the dispatch policy escalates to the `wire-compute` entry in the route chain.

```rust
pub struct WireComputeProvider {
    api_url: String,
    api_token: String,
    node_id: String,
    result_channels: Arc<Mutex<HashMap<String, oneshot::Sender<ComputeResult>>>>,
}

impl WireComputeProvider {
    /// Three-phase call via Wire exchange.
    /// Called from llm.rs provider dispatch, NOT from chain_dispatch.rs.
    ///
    /// (1) match on Wire exchange
    /// (2) fill on Wire (Wire charges deposit, dispatches prompt to provider)
    /// (3) await result delivery via webhook on our tunnel
    pub async fn call(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        model: &str,
        temperature: f32,
        max_tokens: usize,
        max_budget: i64,
        response_format: Option<&serde_json::Value>,
    ) -> Result<LlmResponse> {
        // Phase 1: Match on exchange
        // POST /api/v1/compute/match { model_id, max_budget, input_tokens, latency_preference }
        // Input tokens computed locally via tiktoken BEFORE the Wire call.
        let input_tokens = count_tokens(system_prompt, user_prompt, model);
        let match_result = self.match_job(model, max_budget, input_tokens).await?;

        // Phase 2: Fill (financial only -- Wire charges deposit)
        // POST /api/v1/compute/fill { job_id, input_token_count, relay_count: 0 }
        // NO PROMPTS SENT TO THE WIRE IN THIS CALL.
        let fill_result = self.fill_job(&match_result.job_id, input_tokens, 0).await?;

        // Phase 2b: Submit prompt for Wire-proxied dispatch
        // POST /api/v1/compute/submit-prompt { job_id, system_prompt, user_prompt,
        //   temperature, max_tokens, response_format }
        // For 0-relay: Wire proxies this to the provider's tunnel URL.
        // Wire DOES see the payload for standard tier. Acknowledged.
        self.submit_prompt(
            &match_result.job_id,
            system_prompt, user_prompt,
            temperature, max_tokens,
            response_format,
        ).await?;

        // Phase 3: Register oneshot channel and await result
        let (tx, rx) = oneshot::channel();
        {
            let mut channels = self.result_channels.lock().await;
            channels.insert(match_result.job_id.clone(), tx);
        }

        // Await with timeout from the match result
        let result = tokio::time::timeout(
            Duration::from_secs(match_result.timeout_s),
            rx,
        ).await
            .map_err(|_| anyhow!("Wire compute job timed out after {}s", match_result.timeout_s))?
            .map_err(|_| anyhow!("Result channel closed -- provider may have failed"))?;

        Ok(LlmResponse {
            content: result.content,
            usage: TokenUsage {
                prompt_tokens: result.prompt_tokens,
                completion_tokens: result.completion_tokens,
            },
            generation_id: Some(match_result.job_id),
            actual_cost_usd: None,  // Wire compute uses credits, not USD
            provider_id: Some("wire-compute".into()),
            fleet_peer_id: None,
            fleet_peer_model: None,
        })
    }
}
```

### Dispatch Integration

**Where `wire-compute` fits in the existing dispatch chain:**

The dispatch path is: `dispatch_policy.rs` resolves a route -> `llm.rs` walks the provider preference chain -> each provider type has its own call path.

- **`dispatch_policy.rs`:** New `RouteEntry` with `provider_id: "wire-compute"`. Added to routing rules via the dispatch policy contribution YAML. Typically appears after `fleet` and `ollama-local` in the preference chain:

```yaml
routing_rules:
  - name: build-general
    match_config:
      work_type: build
    route_to:
      - provider_id: fleet
        is_local: true
      - provider_id: ollama-local
        is_local: true
      - provider_id: wire-compute    # market fallback when local is exhausted
      - provider_id: openrouter      # cloud fallback
```

- **`llm.rs`:** New branch in the provider dispatch section (after fleet Phase A filtering, in the pool acquisition loop). When `effective_provider_id == "wire-compute"`:
  1. Skip the normal HTTP provider path entirely
  2. Construct `WireComputeProvider` from config (api_url, api_token, node_id from session state)
  3. Call `wire_compute_provider.call()` with the prompt, model, and parameters
  4. Return the `LlmResponse` with `provider_id: "wire-compute"`

- **NOT in `chain_dispatch.rs`.** That module is a higher-level step dispatcher that routes chain steps to LLM vs mechanical functions. The LLM call path goes through `llm.rs` regardless of which provider serves it. `chain_dispatch.rs` calls `llm::call_model_unified_with_audit_and_ctx`, which internally resolves the dispatch policy and provider chain.

- **The three-phase async call** (match + fill + await webhook) is fundamentally different from the sync HTTP provider path. The chain executor's timeout/retry interaction:
  - The chain executor has its own step timeout (from the chain step definition)
  - `WireComputeProvider.call()` has the Wire job timeout (from `match_result.timeout_s`)
  - The effective timeout is `min(chain_step_timeout, wire_job_timeout)`
  - On timeout: `WireComputeProvider` cancels the Wire job (`POST /compute/cancel`), removes the oneshot channel, returns error
  - The chain executor's error strategy handles retry: if the step is retryable, it will call `WireComputeProvider.call()` again, which creates a NEW match on the exchange (possibly routed to a different provider)

### ACK + Async Result Delivery

**PREREQUISITE. Not optional. Not deferred.** Without this, any job exceeding Cloudflare's ~120s origin timeout on a tunneled provider returns 524. This affects market jobs (which may run on slow models for minutes) and fleet jobs on tunneled providers.

**Current state:** The fleet dispatch handler in `server.rs` (line 1460) processes jobs synchronously -- it enqueues the job, waits for the GPU loop to complete it, and returns the result in the HTTP response. This works for fast jobs but times out on long LLM calls through Cloudflare tunnels.

**Phase 3 pattern:**

Provider receives job -> immediately ACKs (HTTP 202) -> processes on GPU -> POSTs result to Wire (settle endpoint) -> Wire forwards result to requester's `/v1/compute/result-delivery` webhook -> requester resolves oneshot channel -> chain executor task completes.

Implementation:

1. **Provider side (`server.rs` `/v1/compute/job-dispatch`):**
   - Receive job from Wire
   - Verify wire_job_token JWT
   - Enqueue into compute queue
   - Return HTTP 202 with `{ "accepted": true, "job_id": "...", "estimated_start_s": N }`
   - Do NOT await GPU completion in the HTTP handler

2. **GPU processing loop (`main.rs`):**
   - When a market job (`source: "market_received"`) completes:
   - Call `POST /api/v1/compute/start` (filled -> executing transition) BEFORE GPU execution
   - Execute the LLM call
   - On success: call `POST /api/v1/compute/settle` with token counts and latency
   - On failure: call `POST /api/v1/compute/fail` with reason
   - On void (unfilled reservation at front): call `POST /api/v1/compute/void`
   - Settlement metadata only -- NO prompt content, NO result content sent to Wire

3. **Wire side (settle route handler):**
   - After `settle_compute_job` RPC succeeds:
   - Forward the result to the requester's tunnel URL: `POST {requester_tunnel_url}/v1/compute/result-delivery`
   - Include: `job_id`, `result_content` (passed through from provider, not stored), `prompt_tokens`, `completion_tokens`, `finish_reason`
   - On delivery failure: retry 3 times with exponential backoff (1s, 5s, 25s)
   - On all retries exhausted: result is lost. The build step times out on the requester side and the chain executor's error strategy handles retry (re-match, new job). Cost: requester pays twice for that call. Acceptable for launch.

4. **Requester side (`server.rs` `/v1/compute/result-delivery`):**
   - Verify Wire signature on the delivery
   - Look up pending oneshot sender keyed by `job_id` in `WireComputeProvider.result_channels`
   - Send result through the oneshot channel
   - Return 200 OK
   - If no awaiting channel exists (stale result after timeout): log warning, return 200 OK (idempotent), discard result

**This same pattern should also be retrofitted to fleet dispatch** (the existing `handle_fleet_dispatch` in `server.rs`). However, fleet retrofit is a separate change and fleet currently works for jobs under ~90s. Phase 3 builds the ACK+async pattern for market jobs; fleet retrofit can follow.

### Result Delivery Endpoint

`POST /v1/compute/result-delivery` on the requester node. Receives results forwarded by the Wire (or directly from provider in a future relay-chain scenario).

**Request shape:**
```json
{
  "job_id": "uuid",
  "wire_job_token": "jwt-string",
  "result_content": "the LLM output text",
  "prompt_tokens": 1234,
  "completion_tokens": 567,
  "latency_ms": 4200,
  "finish_reason": "stop",
  "wire_signature": "ed25519-signature-of-payload"
}
```

**Auth:** The `wire_job_token` JWT is verified against the Wire's Ed25519 public key (same key used for fleet JWT, already persisted in `session.json`). The JWT payload contains the `job_id` -- must match the request body's `job_id`. This proves the result came from the Wire (or was authorized by the Wire).

**Error handling:**
- Result arrives but no oneshot channel exists: stale result. The requester timed out and cancelled. Log at warn level, return 200 OK (idempotent). The result is discarded.
- Result arrives but oneshot send fails (receiver dropped): same as above. The awaiting task was cancelled.
- Malformed request: return 400.
- Invalid JWT: return 403.

**Implementation in `server.rs`:**
```rust
let result_delivery_route = warp::post()
    .and(warp::path!("v1" / "compute" / "result-delivery"))
    .and(warp::body::json())
    .and(with_state(state.clone()))
    .and_then(handle_compute_result_delivery);

async fn handle_compute_result_delivery(
    body: ComputeResultDelivery,
    state: ServerState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify wire_job_token JWT (same Ed25519 key as fleet)
    // 2. Check job_id in JWT matches body.job_id
    // 3. Look up oneshot sender in WireComputeProvider.result_channels
    // 4. Send result through channel (resolves the awaiting call)
    // 5. Remove channel entry
    // 6. Return 200 OK
}
```

### GPU Processing Loop: Settlement Reporting

After the GPU completes a market job (identified by `source: "market_received"` on the `QueueEntry`):

1. **Before GPU execution:** Call `POST /api/v1/compute/start` with `{ job_id, wire_job_token }`. This triggers `start_compute_job` on the Wire, transitioning the job from `filled` to `executing`. Records `started_at` for accurate GPU timing.

2. **After GPU success:**
   ```
   POST /api/v1/compute/settle
   {
     "job_id": "uuid",
     "wire_job_token": "jwt",
     "prompt_tokens": 1234,
     "completion_tokens": 567,
     "latency_ms": 4200,
     "finish_reason": "stop"
   }
   ```
   NO prompt content. NO result content. The Wire sees only settlement metadata. The result content goes through the Wire transiently (as part of the settle route handler forwarding to the requester's webhook) but is never persisted on the Wire.

3. **After GPU failure:**
   ```
   POST /api/v1/compute/fail
   {
     "job_id": "uuid",
     "wire_job_token": "jwt",
     "reason": "model_error"
   }
   ```

4. **Chronicle records:** `market_settled` or `market_failed` event with `work_item_id` correlation (see Section V).

### DADBEAR Integration -- Requester Side

Requester-side DADBEAR flow for outbound market calls. This is the critical integration that makes market compute durable and crash-recoverable.

**Flow:**

1. Chain executor step needs an LLM call. Dispatch policy resolves to `wire-compute` (fleet exhausted, local GPU exhausted or not available).

2. **Create DADBEAR work item.** Semantic path: `market-call/{chain_name}/{step_name}` (e.g., `market-call/code-mechanical/summarize_layer_0`). Source slug: `compute-market`. The work item captures: model_id, estimated_input_tokens, max_budget, step metadata.

3. **Preview gate.** The DADBEAR preview system (`dadbear_preview.rs`) estimates cost from market surface data:
   - Query the market surface: current pricing for the requested model, queue depths, estimated wait time
   - Cost estimate = `(est_input_tokens * matched_rate_in + est_output_tokens * matched_rate_out) * queue_multiplier_bps / 10000 + reservation_fee`
   - `est_output_tokens` comes from network-observed median for this model (from heartbeat performance profile)
   - This uses **market pricing** (credits), not local inference cost (USD/electricity). The `BudgetDecision` enum already supports this: `AutoCommit` / `RequiresApproval` / `CostLimitHold`
   - New budget fields on DADBEAR config: `max_market_cost_credits` (per-batch), `daily_market_cap_credits` (daily)

4. **Budget decision:**
   - If cost within `max_market_cost_credits`: `AutoCommit`. Work item transitions `compiled -> previewed -> committed`. Dispatch proceeds.
   - If cost exceeds batch limit but within daily cap: `RequiresApproval`. Work item gets a `cost_limit` hold. Operator sees the estimate in the DADBEAR hold events UI and approves/rejects.
   - If cost exceeds daily cap: `CostLimitHold`. DADBEAR slug-level hold freezes all market dispatch for this slug until operator intervenes.

5. **Dispatch:** `WireComputeProvider.call()` with match + fill + submit-prompt + await result. Work item transitions `committed -> dispatched`.

6. **Result:** Webhook delivers result. Work item transitions `dispatched -> completed`. DADBEAR supervisor applies result to the calling chain step.

7. **Crash recovery:** If node restarts mid-await:
   - DADBEAR supervisor's Phase A crash recovery (`recover_in_flight_items`) finds the `dispatched` work item with no completed attempt
   - If `elapsed_secs > SLA_TIMEOUT_SECS`: timeout the attempt, check Wire for job status:
     - **Job completed on Wire:** The result was delivered to our webhook but we crashed before processing it. Fetch result from Wire via `GET /api/v1/compute/job/{id}` (Wire holds result transiently for this purpose). Apply result. Work item -> completed.
     - **Job still executing on Wire:** Re-register the webhook by inserting a new oneshot channel in `WireComputeProvider.result_channels` keyed by the original `job_id`. When the result eventually arrives at `/v1/compute/result-delivery`, it resolves this new channel. Work item stays `dispatched`.
     - **Job failed/voided on Wire:** Mark work item attempt as `failed`. DADBEAR supervisor creates new attempt (if retry budget remains) or marks work item `failed`. Chain executor error strategy handles it.
   - If `elapsed_secs < SLA_TIMEOUT_SECS`: re-register the oneshot channel (the provider may still be working). Let the SLA timeout handle truly dead jobs.

### StepContext for Compute-Served Jobs

Provider-side `StepContext` for market jobs (Law 4 compliance):

```rust
StepContext {
    slug: "compute-market",
    build_id: job_id,       // the Wire job ID
    step_name: "compute-serve",
    depth: 0,
    chain_name: None,       // provider doesn't know requester's chain
    content_type: None,     // provider doesn't know requester's content
}
```

Cache key includes `model + prompt_hash`. If the same prompt arrives from different requesters (or the same requester retries), the cache serves the cached result. The cache is content-addressable (Law 4) so the provider's cache transparently deduplicates identical compute requests across the network.

---

## IV. Frontend Workstream

### ComputeEarningsTracker.tsx

Provider-side earnings dashboard:

- **Credits earned** (session / 24h / all-time) from `ComputeMarketState.total_credits_earned`
- **Jobs completed** (session / 24h / all-time) from `ComputeMarketState.total_jobs_completed`
- **Refund/overage tracking:** Jobs where settlement paid less than deposit (requester got refund) vs jobs where Wire absorbed underage. Shows the estimation accuracy trend.
- **Graph Fund slot visibility:** When `payout = 0` on a Graph Fund rotator slot, the UI must explain why. Display: "This job's payment went to the Graph Fund (2/80 rotator slot)." Read from `graph_fund_slot` flag on the job record (set by the settlement route handler based on rotator position).
- **Earnings breakdown:** Per-model earnings (which models earn the most), per-hour distribution

### Build Settings

Dispatch policy UI gains `wire-compute` as a provider option:

- Checkbox: "Use Wire Compute Market as fallback" (adds `wire-compute` entry to route_to chain)
- Position in preference chain: configurable via drag-and-drop (typically after fleet + local, before openrouter)
- Max budget per call: credits (reads from `economic_parameter` contribution)
- Latency preference: `immediate` / `best_price` / `balanced` dropdown

### Build Preview

Before a pyramid build starts, the preview panel shows cost estimates:

- **Local GPU:** Free (own hardware)
- **Fleet:** Free (same operator)
- **Wire Compute Market:** Estimated X credits based on model pricing, estimated token counts, queue depth
- **OpenRouter:** Estimated $X.XX based on API pricing

The market estimate reads from the market surface data (model pricing + queue multiplier at current depth + network-observed median output tokens). This gives the operator a clear picture of the cost before committing.

---

## V. Chronicle Events This Phase

All events write to `pyramid_compute_events` (the existing chronicle table from the Phase 1 handoff). Each event carries `work_item_id` and `attempt_id` for DADBEAR correlation.

| Event | Side | Fields | Description |
|-------|------|--------|-------------|
| `market_fill` | Requester | job_id, model_id, deposit_charged, relay_count, work_item_id | Requester filled a matched slot |
| `market_dispatched` | Requester | job_id, model_id, estimated_wait_s, work_item_id, attempt_id | Prompt submitted to Wire for proxy dispatch |
| `market_settled` | Provider | job_id, model_id, prompt_tokens, completion_tokens, latency_ms, payout_credits, work_item_id | Provider GPU completed, settlement reported |
| `market_failed` | Provider | job_id, model_id, reason, work_item_id | Provider GPU failed or timed out |
| `market_voided` | Provider | job_id, model_id, work_item_id | Unfilled reservation reached queue front |
| `market_cancelled` | Requester | job_id, model_id, refund_amount, work_item_id | Requester cancelled before execution |
| `market_result_received` | Requester | job_id, model_id, latency_ms, tokens_prompt, tokens_completion, work_item_id, attempt_id | Result webhook delivered and resolved |

Each event includes the standard chronicle fields: `event_source` ("market" for requester, "market_received" for provider), `job_path` (semantic, no UUIDs), `chain_name`, `content_type`, `depth`, `task_label`.

---

## VI. Verification Criteria

Full end-to-end verification:

1. **Credit loop:** Node A pyramid build dispatches a step to Wire compute. Node B's GPU serves it. Credits flow: reservation fee charged from A, paid to B (via rotator). Deposit charged from A, escrowed. Settlement pays B actual cost (via rotator), refunds A the overage. Graph Fund receives payment on 2/80 rotator slots. Verify all ledger entries balance.

2. **Cache:** Same prompt dispatched twice to the same provider hits cache on second call. Provider's `StepContext` cache key (model + prompt_hash) deduplicates. Second call returns faster, settlement shows lower latency.

3. **Build completion:** Node A completes a full pyramid build where some steps were served by Node B via the market. The resulting pyramid is valid (all layers present, all nodes have content, hash chain intact).

4. **DADBEAR lifecycle:** Work items track the full lifecycle on both sides. Requester: `compiled -> previewed -> committed -> dispatched -> completed`. Provider: market job creates work item `source: "market_received"`, transitions through `dispatched -> completed`. Both sides' DADBEAR UIs show the work items with correct states.

5. **Chronicle:** All 7 event types recorded with correct fields. Requester chronicle shows `market_dispatched` + `market_result_received`. Provider chronicle shows `market_settled` (or `market_failed`/`market_voided`). Events correlate via `job_id` and `work_item_id`.

6. **Crash recovery:** Kill requester node mid-build (after a market job is dispatched but before result arrives). Restart node. DADBEAR supervisor detects orphaned work item. If job completed on Wire: fetch result, apply, build resumes. If job still executing: re-register webhook, await result, build resumes. If job failed: mark work item failed, chain executor retries with new market match.

7. **Cost preview:** Before build starts, DADBEAR preview gate shows estimated market cost. Auto-commit within budget. Cost-limit hold when over budget. Operator can approve or reject.

8. **Settlement accuracy:** Over 80 settlement events, verify rotator arm distribution: ~76 payments to provider, ~2 to Wire, ~2 to Graph Fund. Each individual settlement pays 100% to exactly one recipient.

9. **ACK+async:** Market job on a slow model (>120s GPU time) completes successfully through the ACK+async path without Cloudflare 524.

---

## VII. Handoff to Phase 4

**Phase 3 leaves working:**
- Full credit loop (match -> fill -> dispatch -> execute -> settle -> pay -> refund)
- Requester integration via `WireComputeProvider` in the LLM dispatch path
- ACK+async result delivery (no more Cloudflare timeouts for market jobs)
- DADBEAR integration on both sides with crash recovery
- Performance observation recording and aggregation
- Chronicle events for all market lifecycle states
- Frontend: earnings tracker, build settings with wire-compute, cost preview

**Phase 4 adds:** Bridge provider type (OpenRouter relay). A node sells its OpenRouter API access to the network. This requires dollar-to-credit conversion, bridge-dedicated API key isolation, `cloud_relay` privacy indicator, and error code mapping (OpenRouter HTTP codes -> Wire job states). The ACK+async pattern from Phase 3 is critical for bridge jobs (which add relay + OpenRouter latency on top of GPU time).

---

## VIII. Audit Corrections Applied

| Source | Finding | How Applied |
|--------|---------|-------------|
| Theme 1 (DADBEAR) | Market jobs bypass work items, holds, preview, crash recovery | Full DADBEAR integration in Section III: work items on both sides, preview gate for cost estimation, crash recovery with Wire job status checking |
| Theme 3a (Data flow contradiction) | `WireComputeProvider.fill_job()` sends prompts to Wire | Rewritten: fill RPC sends only `input_token_count` + `relay_count`. Separate `submit-prompt` call for Wire-proxied dispatch. Wire sees payload for 0-relay standard tier (acknowledged). |
| Theme 3c (0-relay unspecified) | No explicit spec for launch privacy model | Section III explicitly states: 0-relay uses Wire-proxied dispatch, Wire sees payload for standard tier, matches OpenRouter privacy level |
| Theme 4 (SQL bugs) | model_id filter missing in queue decrements | Added `AND model_id = v_job.model_id` to all 5 RPCs (settle, fail, void, cancel, start) |
| Theme 4 (SQL bugs) | Duplicate operator resolution in settle | Single resolution at top of function, removed duplicate in underage branch |
| Theme 4 (SQL bugs) | No filled->executing transition | Added `start_compute_job` RPC with provider-side trigger before GPU execution |
| Theme 4 (SQL bugs) | No cancel_compute_job | Added full cancel RPC with deposit refund, relay fee refund, queue decrement |
| Audit S11 | Cancel RPC "must be added" but wasn't | Full cancel RPC defined (Section II) |
| Audit S3 | fill RPC must NOT return provider tunnel URL | Confirmed: fill returns only `deposit_charged`, `relay_chain`, `provider_ephemeral_pubkey`, `total_relay_fee` |
| Audit J4 | WireComputeProvider != LlmProvider | Dispatch integration in `llm.rs` (not `chain_dispatch.rs`), new provider branch, not trait shoehorning |
| Audit S16 | Phase 3 dispatch path needs explicit spec | Full dispatch integration section with routing rule YAML, `llm.rs` branch location, timeout interaction |
| Audit 5c | No observation aggregation function | `aggregate_compute_observations` function defined (Section II) |
| Known Issue 6 | Requester restart loses in-flight results | DADBEAR crash recovery checks Wire for job status, re-registers webhooks or fetches completed results |
| Handoff TODO | ACK+async result delivery | Full spec as PREREQUISITE (Section III), not optional |
| Handoff Learning 6 | Cloudflare tunnel ~120s timeout | ACK+async pattern eliminates the timeout for market jobs |
