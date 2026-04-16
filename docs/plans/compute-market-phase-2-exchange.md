# Phase 2: Exchange & Matching

**What ships:** Providers publish offers. Requesters match jobs. Wire mirror active. Jobs flow through the exchange. Market jobs enter the compute queue on the provider side via DADBEAR work items.

**Prerequisites:** Phase 1 (shipped), DADBEAR canonical architecture (shipped)

**Architecture doc:** `GoodNewsEveryone/docs/architecture/wire-compute-market.md` (canonical schemas, credit flow, privacy model)
**Build plan:** `agent-wire-node/docs/plans/wire-compute-market-build-plan.md` (full RPC SQL, table CREATE statements)

---

## I. Overview

Phase 2 turns the Phase 1 local-only compute queue into a market-connected exchange. After this phase:

- A provider node can publish standing offers (model, rates, discount curve) to the Wire.
- An external requester (via curl or future Phase 3 requester integration) can match a job against those offers, creating a reserved slot on the provider's queue.
- The fill RPC accepts input token counts and charges deposits (no prompts to the Wire -- ever).
- The Wire dispatches filled jobs to the provider's tunnel URL.
- The provider receives the job, creates a DADBEAR work item, enqueues to the compute queue, GPU processes, result flows back via the job handler.
- The queue mirror pushes state changes to the Wire on every mutation.
- The market surface endpoint exposes pricing, queue depths, and provider availability.

**What Phase 2 does NOT include:**
- Requester-side integration (WireComputeProvider, chain executor dispatch) -- that is Phase 3.
- Settlement (settle/fail/void RPCs exist from Phase 1 migration but are not called by node code yet) -- Phase 3.
- Bridge operations -- Phase 4.
- Relay chain (relay_count > 0 is rejected) -- future phase.

**Privacy model for this phase:** 0-relay market jobs use Wire-proxied dispatch. The Wire receives the prompt at fill time and forwards it to the provider. This matches the current OpenRouter privacy level (standard tier). The Wire strips requester identity before dispatching. Relay chain (1+ hops, where the Wire never sees the prompt) is stubbed to reject.

---

## II. Wire Workstream

### Migrations

The following tables were already created by Phase 1 migrations. Phase 2 creates NO new tables. Reference `wire-compute-market-build-plan.md` Section II for canonical CREATE TABLE statements.

Tables this phase reads/writes:
- `wire_compute_offers` -- provider standing offers (existing)
- `wire_compute_jobs` -- job lifecycle records (existing)
- `wire_compute_queue_state` -- per-(node, model) queue mirror (existing)
- `wire_compute_observations` -- performance observations (existing)
- `wire_market_rotator` -- rotator arm state (existing)

Phase 2 may require a minor migration to add any columns missed in Phase 1, but the schema is defined and stable.

### RPCs Built This Phase

**1. `match_compute_job`** -- Already defined in Phase 1 migration. Phase 2 activates it via the API route.

Reference: build plan lines 862-1012 for canonical SQL.

Phase 2 notes:
- The 500-token fallback in output estimation (`COALESCE(..., 500)`) is a Pillar 37 violation noted in audit S10/item 10. Replace with an `economic_parameter` contribution lookup for `default_output_estimate_tokens` keyed by model family. Fall back to 500 ONLY if no contribution exists (cold bootstrap).
- `select_relay_chain` is called when `relay_count > 0`. This function does not exist. Phase 2 stubs it: `fill_compute_job` MUST reject `p_relay_count > 0` with an explicit error (`RAISE EXCEPTION 'Relay chain not available -- relay_count must be 0 for standard tier'`).
- The `requester_operator_id != provider_operator_id` self-dealing check (audit Theme 5g) should be added to the matching RPC in this phase. Add after offer selection: `IF v_offer.operator_id = p_requester_operator_id THEN RAISE EXCEPTION 'Self-dealing: cannot match own offers';`

**2. `fill_compute_job`** -- Already defined in Phase 1 migration. Phase 2 activates it.

Phase 2 implementation requirements:
- MUST reject `p_relay_count > 0` (relay chain stubbed).
- For 0-relay jobs: no relay chain returned. `relay_chain` returns `'[]'::jsonb`. `total_relay_fee` returns 0.
- The `v_wire_platform_operator_id` variable is referenced but never declared in the plan's fill RPC (audit Theme 4, SQL bug). Fix: add the same resolution query used in `match_compute_job` at the top of `fill_compute_job`.
- The plan calls `select_relay_chain` twice (once for fee calculation, once for routing info) -- audit notes this is non-deterministic even when it exists. For Phase 2 (relay_count=0 only), both calls are dead code. Guard with the relay_count > 0 rejection at the top.
- `fill_compute_job` MUST NOT return `provider_tunnel_url` to callers (audit S3). The Wire uses the tunnel URL internally for dispatch but never exposes it in the API response.

**3. `compute_queue_multiplier_bps`** -- Already defined in Phase 1 migration. Interpolates the queue discount curve to integer basis points. No changes needed.

**4. `deactivate_stale_compute_offers`** -- Already defined. Called by heartbeat handler or on schedule. The `p_stale_threshold_minutes` parameter has a DEFAULT 5 which is a Pillar 37 concern (audit). The caller should pass the value from an `economic_parameter` contribution. The function signature accepts the parameter so it can be driven externally.

### API Routes Built This Phase

All routes are on the Wire (GoodNewsEveryone). Auth is via the node's API token (same as heartbeat).

**1. `POST /api/v1/compute/offers`**
- Auth: Node API token (resolves to `node_id` and `operator_id`)
- Action: Create or update a standing offer on the exchange
- Body: `{ model_id, rate_per_m_input, rate_per_m_output, reservation_fee, queue_discount_curve, max_queue_depth, provider_type?, context_window?, privacy_capabilities? }`
- RPC: INSERT/UPDATE on `wire_compute_offers` (denormalize `operator_id` from `wire_nodes.agent_id -> wire_agents.operator_id` at creation time)
- Response: `{ offer_id, status }`
- Validation: Dual-gate enforcement (Pillar 21) -- node must meet economic AND reputation thresholds. Check at this endpoint, not in matching RPC.
- `queue_discount_curve` entries must use `multiplier_bps` (integer basis points), NOT float multipliers (Pillar 9, audit finding).

**2. `POST /api/v1/compute/match`**
- Auth: Operator API token
- Action: Find best provider, create reserved slot, charge reservation fee
- Body: `{ model_id, max_budget, input_tokens, latency_preference? }`
- RPC: `match_compute_job`
- Response: `{ job_id, matched_rate_in, matched_rate_out, matched_multiplier_bps, reservation_fee, estimated_deposit, queue_position }`
- Note: `provider_tunnel_url` is NEVER in the response (privacy).

**3. `POST /api/v1/compute/fill`**
- Auth: Operator API token (must match `requester_operator_id` on job)
- Action: Fill a reserved slot with input token count, charge deposit
- Body: `{ job_id, input_token_count, relay_count }`
- RPC: `fill_compute_job`
- Response: `{ deposit_charged, relay_chain, provider_ephemeral_pubkey, total_relay_fee }`
- Phase 2: `relay_count` must be 0 or rejected. `relay_chain` is always `[]`.
- After fill succeeds: the route handler dispatches the job to the provider's tunnel URL (`POST {tunnel_url}/v1/compute/job-dispatch`). This is Wire-internal -- the requester never sees the tunnel URL.

**4. `POST /api/v1/compute/queue-state`**
- Auth: Node API token
- Action: Receive queue state snapshot from node, update `wire_compute_queue_state`
- Body: `{ node_id, seq, model_queues: [{ model_id, total_depth, market_depth, is_executing, est_next_available_s, max_market_depth, max_total_depth }], timestamp }`
- Behavior: Reject if `seq <= current seq` for any `(node_id, model_id)` pair (stale push). UPSERT per model queue.
- Note: Each push costs 1 credit (DD-9 economic gate). Batched by the node (one push per state change window, not per event).

**5. `GET /api/v1/compute/market-surface`**
- Auth: Any authenticated user
- Action: Aggregated view of the market
- Query params: `?model_id=...` (optional filter)
- Response: Per-model aggregation of active offers, pricing ranges, queue depths, provider counts, network-observed performance medians
- Source: JOIN `wire_compute_offers` (WHERE status='active') with `wire_compute_queue_state` and `wire_compute_observations`

### Heartbeat Extension

The existing heartbeat response gains a `compute_market` section. Add to the heartbeat handler in GoodNewsEveryone:

```json
{
  "compute_market": {
    "performance_profile": {
      "<model_id>": {
        "median_tps": <real>,
        "p95_latency_ms": <int>,
        "median_output_tokens": <int>,
        "observation_count": <int>
      }
    },
    "market_summary": {
      "your_completed_jobs_24h": <int>,
      "your_earned_credits_24h": <int>,
      "unfilled_demand": {
        "<model_id>": { "pending_bids": <int>, "avg_budget": <int> }
      }
    },
    "fleet_nodes": [ ... ]
  }
}
```

`performance_profile` comes from `wire_compute_observations` aggregated per node per model. `market_summary` comes from `wire_compute_jobs` filtered by provider_node_id in last 24h. `unfilled_demand` comes from active reserved-but-unfilled jobs grouped by model. `fleet_nodes` already exists from Phase 1.

---

## III. Node Workstream

### compute_market.rs: Full Market State

Replace the Phase 1 stub (`pub struct ComputeMarketState { pub enabled: bool }`) with full market state.

```rust
/// Persisted to compute_market_state.json.
pub struct ComputeMarketState {
    pub offers: HashMap<String, ComputeOffer>,      // model_id -> offer
    pub active_jobs: HashMap<String, ComputeJob>,   // job_id -> in-flight job
    pub total_jobs_completed: u64,
    pub total_credits_earned: i64,                  // Pillar 9: integer
    pub session_jobs_completed: u64,
    pub session_credits_earned: i64,                // Pillar 9: integer
    pub is_serving: bool,
    pub last_evaluation_at: Option<String>,
    pub queue_mirror_seq: HashMap<String, u64>,     // model_id -> monotonic seq
}

pub struct ComputeOffer {
    pub model_id: String,
    pub provider_type: String,                      // "local" | "bridge"
    pub rate_per_m_input: i64,                      // Pillar 9: i64 not u64 (J14)
    pub rate_per_m_output: i64,                     // Pillar 9: i64 not u64 (J14)
    pub reservation_fee: i64,
    pub queue_discount_curve: Vec<QueueDiscountPoint>,
    pub max_queue_depth: usize,
    pub wire_offer_id: Option<String>,              // ID on the Wire exchange
}

pub struct QueueDiscountPoint {
    pub depth: usize,
    pub multiplier_bps: i32,                        // Integer basis points (Pillar 9, audit)
}

pub struct ComputeJob {
    pub job_id: String,
    pub model_id: String,
    pub status: ComputeJobStatus,
    pub messages: Option<serde_json::Value>,         // JSONB, not string fields (J16)
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub wire_job_token: String,
    pub matched_rate_in: i64,                       // Pillar 9: i64
    pub matched_rate_out: i64,                      // Pillar 9: i64
    pub matched_multiplier_bps: i32,                // Integer basis points (Pillar 9)
    pub queued_at: String,
    pub filled_at: Option<String>,
    pub work_item_id: Option<String>,               // DADBEAR correlation
    pub attempt_id: Option<String>,                 // DADBEAR correlation
}
```

Key differences from the plan's stale schema (audit Theme 4, "QueueEntry schema diverged"):
- All credit fields are `i64` not `u64` (J14)
- `multiplier` fields use `_bps: i32` not `f64` (Pillar 9)
- `messages` is `serde_json::Value` not separate `system_prompt`/`user_prompt` strings (J16)
- DADBEAR `work_item_id` and `attempt_id` fields added

### compute_queue.rs: `enqueue_market`

Add a new method to `ComputeQueueManager`. This method differs from `enqueue_local` in critical ways:

```rust
impl ComputeQueueManager {
    /// Add a market job to the specified model's queue.
    /// Unlike enqueue_local (which blocks and never rejects), enqueue_market:
    /// - Respects max_market_depth (rejects with QueueError::DepthExceeded)
    /// - Sets source: "market_received" on the entry
    /// - The caller (job-dispatch handler) must create a DADBEAR work item
    ///   BEFORE calling this method
    /// - Returns the new queue position on success
    pub fn enqueue_market(
        &mut self,
        model_id: &str,
        entry: QueueEntry,
        max_market_depth: usize,
    ) -> Result<usize, QueueError> {
        let queue = self.queues.entry(model_id.to_string())
            .or_insert_with(|| {
                self.round_robin_keys.push(model_id.to_string());
                ModelQueue { entries: VecDeque::new() }
            });

        // Count current market entries in this model's queue
        let current_market_depth = queue.entries.iter()
            .filter(|e| e.source == "market_received")
            .count();

        if current_market_depth >= max_market_depth {
            return Err(QueueError::DepthExceeded {
                model_id: model_id.to_string(),
                current: current_market_depth,
                max: max_market_depth,
            });
        }

        let position = queue.entries.len();
        queue.entries.push_back(entry);
        Ok(position)
    }
}

pub enum QueueError {
    DepthExceeded { model_id: String, current: usize, max: usize },
    ModelNotLoaded { model_id: String },
}
```

The actual `QueueEntry` struct from `compute_queue.rs` (Phase 1, current code):

```rust
pub struct QueueEntry {
    pub result_tx: oneshot::Sender<anyhow::Result<LlmResponse>>,
    pub config: LlmConfig,
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    pub options: LlmCallOptions,
    pub step_ctx: Option<StepContext>,
    pub model_id: String,
    pub enqueued_at: std::time::Instant,
    pub work_item_id: Option<String>,       // DADBEAR correlation
    pub attempt_id: Option<String>,         // DADBEAR correlation
    pub source: String,                     // "local", "fleet_received", "market_received"
    pub job_path: String,                   // semantic chronicle path
    pub chronicle_job_path: Option<String>, // pre-assigned from upstream
}
```

This is the ACTUAL struct. The plan's `QueueEntry` (lines 1306-1313) is completely stale -- it has 5 missing fields, a different structure, and uses an enum `QueueSource` that doesn't exist. Implementers MUST use the struct above.

### Queue Mirror

The queue mirror pushes the node's queue state to the Wire every time it changes. Implementation:

**When to push:** After every `enqueue_local`, `enqueue_market`, and `dequeue_next` call. Also on reconnection (immediate push of current state).

**Batching/debounce:** Use a debounce window read from an `economic_parameter` contribution (key: `queue_mirror_debounce_ms`). Default bootstrap: 500ms. Multiple queue mutations within the window coalesce into a single push. This is important because DD-9 charges 1 credit per push.

**Implementation pattern:**
1. On any queue mutation, send the current snapshot through a `tokio::sync::mpsc` channel to a dedicated mirror task.
2. The mirror task debounces: on receiving a snapshot, wait `debounce_ms`. If another snapshot arrives during the wait, discard the old one, restart the timer.
3. On timer expiry, POST to `/api/v1/compute/queue-state` with the latest snapshot.

**Seq management:**
- `ComputeMarketState.queue_mirror_seq` tracks a monotonic sequence number per model_id.
- Each push increments the seq for every model included.
- The Wire rejects pushes where `seq <= current` for any `(node_id, model_id)`.

**Reconnect push:** When the node registers or re-registers with the Wire (heartbeat reestablishes connection), immediately push current queue state with fresh seq numbers. This ensures the Wire has accurate state after any disconnection.

**Seq conflict recovery:** If the Wire rejects a push (409 or similar indicating stale seq), the node must:
1. Assume its local seq drifted (e.g., after crash + restart where the Wire kept a higher seq from a previous session).
2. Read the current Wire seq for each model via a GET or from the heartbeat response.
3. Set local seq to `wire_seq + 1` and re-push.

**Push failure backoff:** On any push failure (network error, 5xx), use exponential backoff (1s, 2s, 4s, capped at 30s). Record a chronicle event `queue_mirror_push_failed` with the error.

**Snapshot shape** (matches the Wire's `POST /api/v1/compute/queue-state` body):
```rust
pub struct QueueMirrorSnapshot {
    pub node_id: String,
    pub seq: u64,                           // overall push seq
    pub model_queues: Vec<ModelQueueState>,
    pub timestamp: String,                  // ISO 8601
}

pub struct ModelQueueState {
    pub model_id: String,
    pub total_depth: usize,                 // local + market entries
    pub market_depth: usize,                // market entries only
    pub is_executing: bool,
    pub est_next_available_s: Option<u32>,
    pub max_market_depth: usize,
    pub max_total_depth: usize,
}
```

Privacy: `local_depth` and `executing_source` are NOT included (audit J7 -- prevents leaking work patterns).

### server.rs: Job Dispatch Endpoint

`POST /v1/compute/job-dispatch` -- receives matched and filled jobs from the Wire.

This endpoint follows the same pattern as the existing `handle_fleet_dispatch` in `server.rs` (line 1460+), but with Wire JWT auth instead of fleet JWT auth.

**Request body:**
```rust
pub struct ComputeJobDispatchRequest {
    pub job_id: String,
    pub model: String,
    pub messages: serde_json::Value,        // JSONB array of {role, content}
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub response_format: Option<serde_json::Value>,
    pub wire_job_token: String,             // JWT signed by Wire
    pub credit_rate_in_per_m: i64,
    pub credit_rate_out_per_m: i64,
    pub timeout_s: u64,
    pub privacy_tier: String,               // "standard" for Phase 2
}
```

**Handler flow:**

1. **Verify `wire_job_token` JWT** -- signed by Wire's Ed25519 key (same key used for fleet JWT, already persisted in `AuthState`). Validate `aud: "compute"` (distinct from fleet's `aud: "fleet"`).

2. **Check queue capacity** -- call `queue.queue_depth(model_id)` and compare against `max_market_depth` from the offer. Reject with 503 if full.

3. **Create DADBEAR work item** -- see Section V below.

4. **Create QueueEntry** with:
   - `source: "market_received"`
   - `work_item_id: Some(dadbear_work_item_id)`
   - `attempt_id: Some(dadbear_attempt_id)`
   - `job_path: format!("market/{}", job_id)`
   - `chronicle_job_path: Some(format!("market/{}", job_id))`
   - Extract `system_prompt` and `user_prompt` from the `messages` JSONB (first system message, concatenated user messages).
   - `config` with `compute_queue: None` (prevent re-enqueue)

5. **Call `enqueue_market`** -- if `QueueError::DepthExceeded`, return 503.

6. **Notify queue** -- `compute_queue.notify.notify_one()`

7. **Record chronicle event** `market_received` -- see Section III Chronicle Events.

8. **Push queue mirror** -- trigger a mirror push (send snapshot to mirror channel).

9. **Await result via oneshot** -- the `result_tx`/`result_rx` pair on the QueueEntry. The GPU loop processes the job and sends the result back through the oneshot. The handler awaits `result_rx` with a timeout of `timeout_s`.

10. **On result:**
    - Success: return `{ job_id, result_content, prompt_tokens, completion_tokens, latency_ms, finish_reason }` to the Wire (which will call `settle_compute_job`).
    - Timeout/failure: return error to the Wire (which will call `fail_compute_job`).
    - Transition DADBEAR work item to `completed` or `failed`.

**Important:** This handler is synchronous from the Wire's perspective -- it holds the HTTP connection open until the GPU finishes. For Phase 2 this is acceptable because the Cloudflare tunnel has ~120s timeout and most inference calls complete well within that. The 100-year fix (ACK + async result delivery) is a Phase 3 enhancement noted in the handoff doc.

### Offer Management IPC

Tauri IPC commands for the frontend to manage offers:

**`compute_offer_create`**
- Args: `model_id: String, rate_per_m_input: i64, rate_per_m_output: i64, reservation_fee: i64, queue_discount_curve: Vec<QueueDiscountPoint>, max_queue_depth: usize`
- Flow: POST to Wire `/api/v1/compute/offers`, store returned `offer_id` in `ComputeMarketState.offers`, persist to disk.
- Validation: model must be currently loaded (check Ollama status or local_mode config).

**`compute_offer_update`**
- Args: `model_id: String, rates/curve updates`
- Flow: POST to Wire (same endpoint, UPSERT semantics via `UNIQUE(node_id, model_id, provider_type)`), update local state.

**`compute_offer_remove`**
- Args: `model_id: String`
- Flow: DELETE to Wire `/api/v1/compute/offers/:id`, remove from local state.
- Note: Active jobs on this offer continue to completion. Only new matches are prevented.

**`compute_offers_list`**
- Returns current offers from `ComputeMarketState.offers`.

**`compute_market_enable` / `compute_market_disable`**
- Enable: set `is_serving = true`, start queue mirror loop, publish any configured offers.
- Disable: set `is_serving = false`, stop mirror loop, set all Wire offers to `inactive`.

### Chronicle Events This Phase

Each event uses `ChronicleEventContext` (existing struct). Events are recorded via `compute_chronicle::record_event`.

**1. `market_offered`**
- When: Provider successfully publishes an offer to the Wire
- Source: `"market"`
- Fields: `model_id`, `rate_per_m_input`, `rate_per_m_output`, `reservation_fee` in metadata
- job_path: `market/offer/{model_id}`
- work_item_id: None (no DADBEAR work item for offer management)

**2. `market_received`**
- When: Provider receives a matched job from the Wire at `/v1/compute/job-dispatch`
- Source: `"market_received"`
- Fields: `model_id`, `job_id`, `credit_rate_in_per_m`, `credit_rate_out_per_m`, `privacy_tier` in metadata
- job_path: `market/{job_id}`
- work_item_id: the DADBEAR work item ID created for this job
- attempt_id: the DADBEAR attempt ID

**3. `market_matched`**
- When: A requester successfully matches a job (requester-side event)
- Source: `"market"`
- Note: Requester integration is Phase 3. This event type is DEFINED in Phase 2 but the call site ships in Phase 3. Document it now so the chronicle schema is consistent.

**4. `queue_mirror_push_failed`**
- When: Queue mirror push to Wire fails (network error, rejection)
- Source: `"market"`
- Fields: `error`, `seq`, `retry_count` in metadata
- job_path: `market/mirror/{timestamp}`
- work_item_id: None

---

## IV. Frontend Workstream

### ComputeOfferManager.tsx

Location: `src/components/market/ComputeOfferManager.tsx`

Create and edit offers for models currently loaded on this node.

- List current offers with model, rates, discount curve, Wire status
- Create new offer: select from loaded models, set per-M-token input/output rates, reservation fee
- Queue discount curve editor: visual curve with drag points (depth on x-axis, multiplier_bps on y-axis). Show the effective price at each depth level.
- All pricing inputs are integers (basis points for multipliers, credits for rates)
- Wire sync status indicator: show when the offer is active on the Wire vs pending sync
- IPC: `compute_offer_create`, `compute_offer_update`, `compute_offer_remove`, `compute_offers_list`

### ComputeMarketSurface.tsx

Location: `src/components/market/ComputeMarketSurface.tsx`

Browse the network's available compute providers and pricing.

- Fetches from `GET /api/v1/compute/market-surface` (via IPC command `compute_market_surface` which calls the Wire API)
- Per-model view: list of providers offering each model, their rates, queue depths, observed performance
- Filter by model, sort by price/speed/queue depth
- Pricing tier visualization: show how queue discount curves affect effective rates at different depths
- Network-observed performance: median TPS, p95 latency, observation count (confidence indicator)
- Read-only for Phase 2 (no "buy compute" action until Phase 3 requester integration)

### ComputeQueueView.tsx Updates

Location: existing `src/components/market/ComputeQueueView.tsx` (from Phase 1)

Updates for Phase 2:
- Now shows both local and market entries, distinguished by the `source` field on QueueEntry
- Market entries display: `job_id`, `model_id`, credit rates, time in queue
- Local entries display: build context (slug, step_name, depth) as before
- Visual distinction between sources (e.g., different colors or icons)
- `graph_fund_slot` indicator on completed market jobs (when payout went to Graph Fund instead of provider -- audit item 11)

---

## V. DADBEAR Integration

Every market job creates a DADBEAR work item. This is the #1 critical finding from the consolidated audit (Theme 1). The DADBEAR system provides crash recovery, hold checking, attempt tracking, and durable state that raw queue entries lack.

### Provider-Side Flow

1. **Job arrives** at `POST /v1/compute/job-dispatch` from the Wire.

2. **Create DADBEAR work item** in `dadbear_work_items`:
   - `id`: semantic path `"market/{job_id}"` (no UUIDs per handoff rule 7)
   - `slug`: `"compute-market"` (virtual slug for market work -- not a pyramid slug)
   - `batch_id`: the Wire `job_id` (groups this job's lifecycle)
   - `epoch_id`: current timestamp-based epoch
   - `step_name`: `"compute-serve"`
   - `primitive`: `"llm_call"`
   - `layer`: 0
   - `target_id`: the Wire `job_id`
   - `system_prompt`: extracted from `messages` JSONB
   - `user_prompt`: extracted from `messages` JSONB
   - `model_tier`: the requested model
   - `state`: `"compiled"` (enters compiled, preview gate evaluates next)

3. **Create work attempt** via `create_work_attempt` (existing function in `dadbear_supervisor.rs`):
   - Links to the work item
   - Tracks dispatch timing and outcome

4. **Hold check** (via DADBEAR holds projection):
   - Check for active holds on the `"compute-market"` slug: `frozen` (operator paused market), `breaker` (quality system flagged this node), `cost_limit` (credit balance too low to absorb potential settlement risk).
   - If any hold is active: reject the job (return 503 to Wire), transition work item to `"blocked"`, record chronicle event.
   - If no holds: proceed.

5. **Enqueue** -- call `enqueue_market` with the `QueueEntry`:
   - `source: "market_received"`
   - `work_item_id: Some("market/{job_id}")`
   - `attempt_id: Some(attempt_id_from_step_3)`
   - `job_path: "market/{job_id}"`

6. **GPU loop processes** -- the GPU processing loop (already built in Phase 1) is market-agnostic. It pulls from the queue, calls `call_model_unified_with_options_and_ctx`, sends the result through the oneshot channel. No changes needed to the GPU loop.

7. **Result flows back** through the oneshot channel to the `handle_compute_job_dispatch` handler, which:
   - On success: returns result to Wire, Wire calls `settle_compute_job` (Phase 3 activation)
   - On failure: returns error to Wire, Wire calls `fail_compute_job`

8. **DADBEAR work item transitions:**
   - On success: `dispatched -> completed` (via CAS transition)
   - On failure: `dispatched -> failed` (via CAS transition)
   - The supervisor's result application logic does NOT apply here (market jobs don't modify pyramids on the provider side -- they just run inference). The work item lifecycle is for crash recovery and audit trail.

### Crash Recovery

If the node crashes while a market job is in-flight:
- The DADBEAR supervisor's crash recovery scan (runs on startup) finds work items in `dispatched` state with expired SLA.
- For market work items (slug `"compute-market"`): transition to `failed`. The Wire's timeout sweep (`sweep_timed_out_compute_jobs`) will independently fail the job and refund the requester.
- No re-dispatch of market jobs on provider restart (the Wire handles retry by matching a new job if the requester retries).

### Why Not Just Use the Queue

The queue's `QueueEntry` is ephemeral (in-memory, lost on crash). DADBEAR work items are durable (SQLite, survives crashes). Without DADBEAR:
- Crash during inference = paid job with no result and no recovery signal
- No hold checking = breaker-held provider keeps serving bad results
- No attempt tracking = no audit trail for quality disputes
- No supervisor visibility = market jobs are invisible to the DADBEAR dashboard

---

## VI. Verification Criteria

All verification is scoped to what's testable WITHOUT Phase 3 requester integration. The requester side does not exist yet. Testing uses manual curl or a test script.

1. **Provider publishes offers via IPC:**
   - Call `compute_offer_create` from the frontend
   - Offer appears on the Wire (query `wire_compute_offers` directly or via `GET /api/v1/compute/market-surface`)
   - ComputeOfferManager shows the offer with Wire sync status

2. **Manual match creates a job record:**
   - `curl -X POST {wire_url}/api/v1/compute/match -d '{"model_id": "...", "max_budget": 1000, "input_tokens": 500}'`
   - Returns `job_id`, rates, queue position
   - `wire_compute_jobs` has a row with status `reserved`
   - Reservation fee charged from requester operator balance

3. **Manual fill transitions job to filled:**
   - `curl -X POST {wire_url}/api/v1/compute/fill -d '{"job_id": "...", "input_token_count": 500, "relay_count": 0}'`
   - Returns `deposit_charged`, empty relay chain
   - `wire_compute_jobs` status changes to `filled`
   - Deposit charged from requester balance
   - `relay_count > 0` is rejected with error

4. **Wire dispatches job to provider:**
   - After fill, Wire POSTs to provider's tunnel URL `/v1/compute/job-dispatch`
   - Provider receives the job
   - DADBEAR work item created in `dadbear_work_items` with `slug = "compute-market"`, `state = "compiled"`

5. **Provider enqueues and GPU processes:**
   - Job appears in compute queue (visible in ComputeQueueView with `source: "market_received"`)
   - GPU loop picks it up, runs inference
   - Result flows back to the job-dispatch handler
   - Handler returns result to Wire

6. **Queue mirror pushes state changes:**
   - After enqueue: queue mirror pushes updated depths to Wire
   - `wire_compute_queue_state` reflects current local state
   - Verify debounce: rapid mutations produce batched pushes, not one per mutation
   - Verify seq monotonicity and stale rejection

7. **Market surface shows offers:**
   - `GET /api/v1/compute/market-surface` returns the provider's offers with pricing, queue depths
   - ComputeMarketSurface component renders the data

8. **Chronicle records market_received event:**
   - After job receipt, `pyramid_compute_events` has a row with `event_type = "market_received"`, `source = "market_received"`, `work_item_id = "market/{job_id}"`
   - Chronicle tab in frontend shows the event

9. **DADBEAR work item lifecycle:**
   - Work item transitions: `compiled -> dispatched -> completed` (or `failed`)
   - Work attempt row tracks timing
   - DADBEAR dashboard shows market work items

10. **Self-dealing rejection:**
    - Attempt to match own offers: `match_compute_job` with `requester_operator_id = provider_operator_id`
    - Rejected with error

11. **Depth limit enforcement:**
    - Fill queue to `max_market_depth` with market jobs
    - Next market job is rejected at `enqueue_market` (provider returns 503)
    - Local jobs still enqueue (enqueue_local blocks but never rejects)

---

## VII. Handoff to Phase 3

**What Phase 2 leaves working:**
- Provider-side exchange infrastructure: offers published on Wire, jobs received and processed, DADBEAR work items tracking everything
- Wire-side matching and fill RPCs: functional with relay stubbed (relay_count=0 only)
- Queue mirror: active, pushing state on every change, debounced, seq-managed
- Market surface: browseable, shows offers/pricing/depths
- Chronicle: recording market_received events on provider side
- Job dispatch endpoint: receiving and processing jobs synchronously

**What Phase 3 adds:**
- **Requester-side integration:** `WireComputeProvider` as a new dispatch provider in the chain executor. The chain executor can dispatch LLM calls to the Wire compute market instead of (or in addition to) local/OpenRouter/fleet.
- **Settlement activation:** The node's job-dispatch handler reports completion metrics to the Wire. Wire calls `settle_compute_job`, `fail_compute_job`, `void_compute_job`. Credit flow completes.
- **Result delivery:** ACK + async pattern to handle Cloudflare 120s timeout. Provider ACKs job receipt immediately, POSTs result back to Wire when GPU finishes. Wire pushes result to requester's tunnel URL.
- **Cancel RPC:** `cancel_compute_job` for requester to cancel reserved/filled (not executing) jobs (audit S11).
- **`filled -> executing` transition:** Status transition when GPU starts processing (currently undefined -- audit Theme 4).
- **Requester-side chronicle events:** `market_matched`, `market_fill`, `market_dispatched`, `market_settled`, `market_failed`, `market_voided`.
- **Requester-side DADBEAR:** Outbound market calls go through DADBEAR preview gate for cost estimation.

---

## VIII. Audit Corrections Applied

| Audit Finding | How Addressed |
|---|---|
| Theme 1 (DADBEAR absent from Phase 2) | Section V: Full DADBEAR integration. Every market job creates a work item with semantic path ID, attempt tracking, hold checking, crash recovery. |
| Theme 2 (Chronicle absent) | Section III Chronicle Events: `market_offered`, `market_received`, `queue_mirror_push_failed` defined with fields. `market_matched` defined but ships in Phase 3. |
| Theme 3a (fill sends prompts to Wire) | Phase 2 uses 0-relay Wire-proxied dispatch. Fill RPC sends only `input_token_count`, NOT prompts. Prompts are dispatched separately by the Wire route handler to the provider tunnel. |
| Theme 3b (select_relay_chain undefined) | fill_compute_job rejects relay_count > 0. select_relay_chain is dead code in Phase 2. |
| Theme 3c (0-relay flow unspecified) | Explicitly specified: Wire-proxied dispatch for standard tier. Wire sees payloads (acceptable, matches OpenRouter privacy). |
| Theme 4 (QueueEntry schema diverged) | Section III: actual QueueEntry struct from compute_queue.rs used verbatim. Plan's stale struct explicitly called out. |
| Theme 4 (model_id filter missing in RPCs) | Noted: settlement/fail/void RPCs need `AND model_id = v_job.model_id` on queue state UPDATE. Wire migration fix required. |
| Theme 4 (v_wire_platform_operator_id undeclared in fill) | Noted: add operator resolution query to fill_compute_job. |
| Theme 4 (duplicate operator resolution in settlement) | Noted: remove duplicate query in settle_compute_job. |
| Theme 4 (no filled->executing transition) | Deferred to Phase 3 (when settlement activates). Noted in handoff. |
| Theme 5g (self-dealing) | Added to match_compute_job: reject requester_operator_id = provider_operator_id. |
| Pillar 37 (500-token fallback) | Replace with economic_parameter contribution lookup. 500 only as bootstrap fallback. |
| Pillar 9 (f64 in credits/multipliers) | All credit fields i64. All multiplier fields integer basis points (_bps). No float in financial paths. |
| J7 (queue mirror leaks patterns) | Mirror snapshot excludes local_depth and executing_source. |
| J14 (u64 credits in Rust) | Changed to i64 throughout ComputeMarketState. |
| J16 (system_prompt/user_prompt -> messages) | ComputeJob uses messages: serde_json::Value. |
| S3 (fill returns tunnel URL) | fill_compute_job must NOT return provider_tunnel_url. |
| S11 (cancel RPC missing) | Deferred to Phase 3. Noted in handoff. |
| S13 (heartbeat doesn't refresh queue state) | Heartbeat handler should update wire_compute_queue_state.updated_at as fallback liveness signal. |
| Item 10 (graph_fund_slot visibility) | ComputeQueueView shows graph_fund_slot indicator on completed market jobs. |
