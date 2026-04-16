# Wire Compute Market -- Architecture Reference

**Date:** 2026-04-15 (revised from 2026-04-13 original)
**Companion docs:** Per-phase implementation docs (`compute-market-phase-{2-6}.md`), seams doc
**Codebase verified against:** `compute_queue.rs`, `compute_market.rs`, `dadbear_compiler.rs`, `dadbear_supervisor.rs`, `dadbear_preview.rs`

---

## I. Core Principles

1. **Order book, not centralized routing.** Providers publish standing offers (asks). Requesters submit jobs (bids). The exchange matches when bid >= ask. The Wire doesn't decide routing -- the price does.

2. **Per-model FIFO queues.** Each loaded model on a node has its own independent queue. Local builds and market jobs share each model's queue. No priority classes, no preemption. Pricing IS priority. Nodes with multiple models loaded have multiple independent queues with independent depths, pricing curves, and throughput.

3. **Network-observed performance.** Nodes never self-report throughput. The network measures actual delivery times from completed jobs, segmented by model, input size, and output size. This data flows to both the node and the market.

4. **Individual calls as atomic unit.** Each queue operates on single LLM calls. Batches are composed of individual calls. Steps register when they're actually needed (pyramid builds are emergent).

5. **Serial GPU execution per model.** Default concurrency: 1 per model queue. Queue feeds one job at a time. Owner can override. Most stable, most predictable. Multiple model queues may run concurrently if hardware supports it.

6. **Two-part pricing through Wire escrow.** Reservation fee (fixed, non-refundable, to provider immediately) + per-token metered cost (estimated on fill, escrowed by Wire, settled against actual on completion).

7. **Queue mirror.** Queues exist locally regardless. When the node goes online for compute market, the Wire gets a mirror. Two sources add to each model's queue (local + market). Both sides see the same state.

8. **Push everywhere, pull nowhere.** All result delivery via webhook to tunnel URLs. The Wire pushes results to requesters. Providers push results to Wire. No polling. Every node has a tunnel.

9. **Fleet-first routing.** Nodes under the same operator (Wire account) route to each other directly -- no Wire proxy, no credits, no settlement. Fleet traffic is completely private (bypasses the Wire). The exchange is only used when fleet capacity is exhausted.

10. **Zero hardcoded numbers.** Every parameter the exchange reads is a contribution: pricing, curves, levy rates, thresholds, timeouts, deposit percentages, matching weights. The exchange is a mechanism that reads contributions and applies them. Pillar 37 absolute.

---

## II. Credit Flow

All compute market credit flows go through the Wire as clearing house:

```
Requester --> Wire (reservation fee + token deposit)
Wire --> Provider (reservation fee immediately, actual token cost on settlement)
Wire --> Graph Fund (2.5% of actual token cost via rotator arm)
Wire --> Requester (refund if estimate > actual)
Wire platform absorbs underage (if actual > estimate -- requester NEVER pays more than deposit)
```

### Rotator Arm Mechanics

Service payments, not contribution royalties. No creator/source-chain split (no UFF 60/35). But Wire 2.5% + Graph Fund 2.5% both apply -- the Wire provides the exchange, proxy, escrow, and settlement infrastructure. Provider receives 95%. Implemented via rotator arm: 76 provider slots, 2 Wire slots, 2 Graph Fund slots out of 80.

**How the rotator arm works:** An 80-position Bjorklund cycle. Each settlement advances the rotator by 1. On a Wire/GF slot (positions determined by Bjorklund even distribution), the FULL actual_cost goes to that recipient; the provider gets zero for that job. Over 80 jobs, the provider receives exactly 76/80 = 95% of total revenue. Pure integer economics -- no percentage math, no rounding.

**Slot counts are contribution-driven** via the `market_rotator_config` economic_parameter. The 76/2/2 distribution is the seed value, not a hardcoded constant.

**Separate rotators for settlement and reservation fees.** Each (node_id, market_type, scope_id, rotator_type) tuple has its own rotator position. Compute settlement and compute reservation cycle independently.

### Wire Platform Operator (`agentwireplatform`)

Absorbs estimation risk. When actual cost > deposit estimate, the Wire platform operator is debited the difference. The requester NEVER pays more than the deposit amount. This entity's balance may go negative -- replenished from platform revenue (credit purchases, Graph Fund overflow). Used ONLY for `compute_estimation_subsidy` debits.

### Graph Fund (`agentwiregraphfund`)

Receives 2.5% via rotator arm (2/80 slots). Resolved by handle at settlement time via standard handle resolution -- not a hardcoded UUID.

---

## III. Privacy Model

**IMPORTANT: The privacy model at launch is HONEST about what each party sees.**

### Standard Tier (Launch -- 0-relay market jobs)

At launch, market jobs with `relay_count=0` use **Wire-proxied dispatch**. This means the Wire acts as intermediary for the data plane. This matches current OpenRouter privacy level.

| Party | What they see |
|-------|---------------|
| **Wire** | Prompt content, result content, requester identity, provider identity, all metadata |
| **Provider** | Prompt content, model, parameters, job token. Does NOT see requester identity, build context, pyramid slug, layer, step name |
| **Requester** | Result content. Does NOT see provider identity (tunnel URL rotates) |

This is acceptable for standard tier. The Wire is trusted infrastructure the same way OpenRouter is today.

### Relay Tier (Future -- relay_count > 0)

When relay infrastructure ships, market jobs with `relay_count > 0` route through relay nodes:

```
Requester --> Relay A --> ... --> Relay N --> Provider
```

| Party | What they see |
|-------|---------------|
| **Wire** | Matching metadata, settlement data. NEVER sees prompt content or inference results |
| **Provider** | Prompt content (must -- they run inference), model, parameters, job token. Does NOT see requester identity (sees last relay's tunnel URL) |
| **Relays** | Ciphertext only (E2E encrypted between requester and provider) |
| **Requester** | Result content. Does NOT see provider identity |

At launch, `select_relay_chain` is stubbed to reject `relay_count > 0`.

### Bridge Tier (`cloud_relay`)

Bridge offers MUST carry `privacy_capabilities: '{cloud_relay}'` -- NOT `'{standard}'`. Prompts flow through: bridge node --> OpenRouter --> upstream provider. The requester dispatch policy must support filtering by privacy capability so requesters can opt out of bridge providers.

| Party | What they see |
|-------|---------------|
| **Bridge node** | Prompt content (forwards to OpenRouter) |
| **OpenRouter** | Prompt content (standard OpenRouter privacy applies) |
| **Upstream LLM provider** | Prompt content |

### Fleet Tier (Completely Private)

Fleet routing bypasses the Wire entirely. Direct node-to-node over tunnel URLs.

| Party | What they see |
|-------|---------------|
| **Wire** | Nothing. Fleet traffic is invisible to the network |
| **Fleet peer** | Prompt content, requester identity (same operator) |

### Future Privacy Tiers (Stubbed)

- **Clean Room:** Ephemeral Docker container on provider, encrypted I/O, provider never sees plaintext.
- **Vault / SCIF:** Wire-owned or Wire-audited hardware. Zero trust chain.

---

## IV. Job Lifecycle State Machine

```
              match_compute_job
                    |
                    v
             [ reserved ]
              /    |    \
  cancel     /     |     \ void (unfilled slot reaches front)
  RPC       /      |      \ void_compute_job RPC
           v       |       v
     [cancelled]   |    [void]
                   |
            fill_compute_job
                   |
                   v
              [ filled ]
              /        \
  cancel    /          \ start_compute_job
  RPC      /            \ (provider confirms GPU start)
          v              v
    [cancelled]     [ executing ]
                    /           \
                   /             \
        fail_compute_job    settle_compute_job
        (or timeout sweep)       |
                |                v
                v          [ completed ]
           [ failed ]
```

### State Transitions

| From | To | Trigger | Who | RPC/Endpoint | What Happens |
|------|-----|---------|-----|-------------|--------------|
| (none) | reserved | Match found | Wire (on requester request) | `match_compute_job` RPC via `POST /api/v1/compute/match` | Reservation fee charged from requester, paid to provider (via rotator arm). Queue depth incremented. Job row created. |
| reserved | filled | Requester submits token count | Requester | `fill_compute_job` RPC via `POST /api/v1/compute/fill` | Deposit charged from requester. Relay chain selected (stubbed). Provider ephemeral pubkey returned. Job updated with input_token_estimate. |
| filled | executing | Provider confirms GPU start | Provider | `start_compute_job` RPC via `POST /api/v1/compute/start` | `dispatched_at` set. Status updated. **This transition was missing from the original plan.** |
| executing | completed | Provider reports result | Provider | `settle_compute_job` RPC via `POST /api/v1/compute/settle` | Actual cost calculated. Provider paid via rotator arm. Deposit overage refunded to requester. Wire absorbs underage. Observation recorded. Queue depth decremented. |
| executing | failed | Provider timeout/error | Provider or sweep | `fail_compute_job` RPC via `POST /api/v1/compute/fail` | Deposit refunded to requester. Reservation fee stays with provider. Queue depth decremented. Failure observation recorded. |
| filled | failed | Timeout | Timeout sweep | `sweep_timed_out_compute_jobs` | Same as executing-->failed. |
| reserved | void | Unfilled slot at queue front | Provider | `void_compute_job` RPC via `POST /api/v1/compute/void` | No deposit was charged. Reservation fee stays with provider. Queue depth decremented. |
| reserved | cancelled | Requester cancels before fill | Requester | `cancel_compute_job` RPC via `POST /api/v1/compute/cancel` | Reservation fee NOT refunded (provider held capacity). Queue depth decremented. |
| filled | cancelled | Requester cancels before execution | Requester | `cancel_compute_job` RPC via `POST /api/v1/compute/cancel` | Deposit refunded. Reservation fee NOT refunded. Queue depth decremented. |

### Failure Recovery

- **Requester crash mid-job:** Result delivered via webhook to tunnel URL. If tunnel is down, Wire retries 3x with backoff. If all retries fail, result is lost. Build step times out on requester side; chain executor error strategy retries (new match, new job). Cost: requester pays twice for that call.
- **Provider crash mid-execution:** Timeout sweep catches it. `fail_compute_job` refunds deposit. Requester retries via chain executor.
- **Wire crash:** RPCs are atomic (single transaction). Partial execution is impossible. On restart, `sweep_timed_out_compute_jobs` catches any jobs stuck in executing/filled past their timeout.

---

## V. Queue Architecture

### Actual QueueEntry Struct (from `compute_queue.rs`)

The current codebase `QueueEntry` struct has the following fields:

```rust
pub struct QueueEntry {
    pub result_tx: oneshot::Sender<anyhow::Result<LlmResponse>>,
    pub config: LlmConfig,           // with compute_queue: None to prevent re-enqueue
    pub system_prompt: String,
    pub user_prompt: String,
    pub temperature: f32,
    pub max_tokens: usize,
    pub response_format: Option<serde_json::Value>,
    pub options: LlmCallOptions,
    pub step_ctx: Option<StepContext>, // Law 4: every LLM call gets StepContext
    pub model_id: String,             // queue routing key (model id or "default")
    pub enqueued_at: std::time::Instant,
    // DADBEAR integration fields
    pub work_item_id: Option<String>, // correlates queue results to durable work items
    pub attempt_id: Option<String>,   // DADBEAR attempt ID for this dispatch
    // Source tracking
    pub source: String,               // "local" | "fleet_received" | (Phase 2+: "market_received")
    pub job_path: String,             // semantic path for chronicle event grouping
    pub chronicle_job_path: Option<String>, // pre-assigned path from upstream handlers
}
```

**Key differences from the plan's stale pseudocode:**
- No `QueueSource` enum -- `source` is a plain `String`
- No `QueuePayload` enum -- prompt fields are always present (reserved slots are a Wire-side concept, not a queue concept)
- `work_item_id` and `attempt_id` fields exist for DADBEAR correlation
- `chronicle_job_path` exists for maintaining job_path continuity across fleet dispatch
- `result_tx` is a oneshot channel, not a webhook callback
- No `estimated_gpu_time_s` -- not tracked at the queue level

### Queue Manager

```rust
pub struct ComputeQueueManager {
    queues: HashMap<String, ModelQueue>,
    round_robin_keys: Vec<String>,
    round_robin_index: usize,
}
```

Per-model FIFO with round-robin draining across models. GPU loop consumes entries one at a time, round-robin for fairness so no single model starves.

### Two Entry Points

- **`enqueue_local(model_id, entry)`:** Current implementation. Used for own builds, stale checks, fleet-received work. No credits. Enters at back of model's queue.
- **`enqueue_market(model_id, entry)` (Phase 2 addition):** For exchange-matched jobs. Credits flow. Enters at back of model's queue. Must respect `max_market_depth` policy limit. Functionally identical to `enqueue_local` but with market-specific source tracking and depth enforcement.

### How DADBEAR Work Items Thread Through the Queue

The DADBEAR supervisor creates `QueueEntry` instances with `work_item_id` and `attempt_id` populated. When the GPU loop completes a call, the result flows back through the oneshot channel to the supervisor's `JoinSet`, which handles:
1. Writing the result to `dadbear_work_attempts`
2. CAS transitioning the work item: dispatched --> completed
3. Applying the result (supersession, cascade observations)
4. CAS transitioning: completed --> applied

Market jobs will follow the same pattern but with `source: "market_received"` and market-specific result handling (settlement report to Wire instead of local application).

---

## VI. DADBEAR Integration Pattern

**This section is new -- the original plan predates DADBEAR. This is the biggest architectural gap found by the audit.**

DADBEAR provides the canonical lifecycle for all durable work on the node: observe --> compile --> preview --> dispatch --> apply. Market jobs MUST flow through DADBEAR work items on both the provider and requester sides.

### DADBEAR Architecture (Current Code)

The DADBEAR system consists of four modules:

1. **Compiler** (`dadbear_compiler.rs`): Reads observation events, maps event_type to primitives, dedup checks, creates work item rows in `compiled` state with semantic path IDs (`{slug}:{epoch_short}:{primitive}:{layer}:{target_id}`). Does NOT make LLM calls.

2. **Preview** (`dadbear_preview.rs`): Batch-level dispatch preview with cost estimates, routing resolution, and policy snapshot. Budget enforcement: auto-commit / requires-approval / cost-limit-hold. 5-minute TTL. CAS transitions: compiled --> previewed.

3. **Supervisor** (`dadbear_supervisor.rs`): 5-second reconciliation tick loop. Crash recovery on startup (scan for dispatched items with no completed attempt). Dispatches previewed items through the compute queue. Handles result application (supersession, cascade observations). CAS state machine: compiled --> previewed --> dispatched --> completed --> applied.

4. **Extend** (`dadbear_extend.rs`): Source folder watcher, ingest dispatcher, session boundary detection. Fires the observation + compilation pipeline that feeds the supervisor.

### Work Item States

```
compiled --> previewed --> dispatched --> completed --> applied
                |                              |
                v                              v
            blocked (holds)                 failed
                |
                v (hold cleared)
          compiled or previewed
```

Additional terminal states: `stale` (epoch rotation or target gone), `timeout` (SLA breach during crash recovery).

### Provider Side (Receiving Market Jobs)

```
Wire dispatches job to provider tunnel URL
    |
    v
Provider creates DADBEAR observation event (source: "market_received")
    |
    v
Compiler maps to work item (primitive: "compute_serve", step: "market_serve")
    |
    v
Preview gate evaluates (auto-commit for market jobs -- provider already accepted the offer)
    |
    v
Supervisor dispatches to compute queue (source: "market_received")
    |
    v
GPU loop executes LLM call
    |
    v
Result flows back to supervisor via oneshot channel
    |
    v
Supervisor applies result:
  - Reports settlement metadata to Wire (POST /api/v1/compute/settle)
  - Writes chronicle event (market_settled)
  - CAS: completed --> applied
```

### Requester Side (Sending Market Jobs)

```
Chain executor needs an LLM call, fleet capacity exhausted
    |
    v
Dispatch policy creates DADBEAR work item (source: "market_dispatch")
    |
    v
Preview gate evaluates:
  - Estimates cost from market pricing (not local cost model)
  - Checks budget limits
  - Auto-commit or hold for operator approval
    |
    v
Supervisor dispatches: calls match_compute_job, fill_compute_job
    |
    v
Sends encrypted prompt through relay chain (or Wire proxy for 0-relay)
    |
    v
Awaits result via webhook (POST {tunnel}/v1/compute/result-delivery)
    |
    v
Result received:
  - CAS: dispatched --> completed
  - Apply: feed result back to chain executor's waiting step
  - Write chronicle event (market_dispatched, market_settled)
  - CAS: completed --> applied
```

### Crash Recovery

The supervisor runs crash recovery on startup BEFORE entering the normal tick loop:
- Scans for `dispatched` work items with no completed attempt
- If elapsed > SLA_TIMEOUT_SECS (300s): marks attempt as `timeout`, transitions work item back to `previewed` (or `compiled` if preview expired), allowing re-dispatch
- If elapsed < SLA_TIMEOUT_SECS: skips (call may still complete)

**Market-specific crash recovery (Phase 2 addition):**
- Provider side: orphaned market work items (dispatched, no result) are detected. If the Wire job is still active, re-dispatch. If the Wire job timed out, mark as failed.
- Requester side: orphaned market dispatches (match was made, result never received). Check Wire job status. If completed, fetch result. If failed/void, mark locally as failed and let chain executor retry.

### Holds

DADBEAR holds are append-only events projected to current state. Three hold types:

- **frozen:** Operator explicitly pauses all work for a slug.
- **breaker:** System-detected issue (e.g., repeated failures, quality problem).
- **cost_limit:** Preview cost exceeds daily budget.

**Market-specific holds (Phase 2 addition):**
- Wire quality holds propagated via heartbeat: if the Wire places a quality hold on a provider's offers (upheld challenge), the heartbeat response includes the hold. The node creates a local DADBEAR breaker hold, suspending market serving until the hold clears.

### Chronicle Integration

All market events carry `work_item_id` and `attempt_id` for DADBEAR correlation. The chronicle columns already exist for these fields.

---

## VII. Chronicle Market Events

| Event Type | Source | When | Key Fields |
|------------|--------|------|------------|
| `market_offered` | Provider | Offer created/updated on Wire | `model_id`, `rate_per_m_input`, `rate_per_m_output`, `offer_id` |
| `market_matched` | Requester | Job matched on exchange | `job_id`, `model_id`, `provider_node_id` (anonymized), `matched_rate`, `queue_depth` |
| `market_fill` | Requester | Slot filled with token count | `job_id`, `input_token_count`, `deposit_amount`, `relay_count` |
| `market_dispatched` | Requester | Prompt sent through relay chain | `job_id`, `work_item_id`, `attempt_id` |
| `market_received` | Provider | Job received from Wire | `job_id`, `model_id`, `work_item_id`, `attempt_id` |
| `market_executing` | Provider | GPU starts processing | `job_id`, `work_item_id` |
| `market_settled` | Both | Settlement complete | `job_id`, `actual_cost`, `provider_payout`, `requester_refund`, `latency_ms`, `tokens_in`, `tokens_out` |
| `market_failed` | Both | Job failed/timed out | `job_id`, `reason`, `work_item_id` |
| `market_voided` | Provider | Unfilled slot resolved | `job_id` |
| `market_cancelled` | Requester | Job cancelled before execution | `job_id`, `deposit_refunded` |
| `bridge_dispatched` | Provider | Bridge job sent to OpenRouter | `job_id`, `openrouter_model`, `work_item_id` |
| `bridge_returned` | Provider | Bridge result received from OpenRouter | `job_id`, `openrouter_cost_usd`, `latency_ms` |
| `bridge_failed` | Provider | Bridge call failed | `job_id`, `openrouter_error_code`, `error_message` |
| `bridge_cost_recorded` | Provider | Dollar cost recorded for credit reconciliation | `job_id`, `usd_cost`, `credit_revenue` |
| `fleet_dispatched` | Requester | Job sent to fleet peer | `peer_node_id`, `model_id`, `work_item_id` |
| `fleet_received` | Provider | Job received from fleet peer | `peer_node_id`, `model_id`, `work_item_id` |
| `fleet_completed` | Both | Fleet job completed | `peer_node_id`, `latency_ms`, `tokens_out` |
| `queue_state_pushed` | Provider | Queue mirror updated on Wire | `model_id`, `total_depth`, `market_depth`, `seq` |
| `offer_deactivated` | Wire | Offer marked offline (stale heartbeat) | `offer_id`, `node_id`, `reason` |

---

## VIII. Wire-Side Schema

### Prerequisites Migration

Before the compute market tables:

**1. Extend `wire_graph_fund.source_type` CHECK:**

Include `'compute_service'`, `'compute_reservation'`, `'storage_serve'`, `'hosting_grant'`, `'relay_hop'`. This is the ONE consolidated migration -- storage and relay plans reference this.

**2. Resolve `operator_id` path:**

`wire_nodes.agent_id` --> `wire_agents.operator_id` --> `wire_operators.id`. Denormalized on offer rows for query performance.

**3. Create rotator arm infrastructure:**

```sql
CREATE TABLE wire_market_rotator (
  node_id       UUID NOT NULL REFERENCES wire_nodes(id),
  market_type   TEXT NOT NULL,       -- 'compute' | 'storage' | 'relay'
  scope_id      TEXT NOT NULL,       -- model_id for compute, 'default' for storage/relay
  rotator_type  TEXT NOT NULL,       -- 'settlement' | 'reservation'
  position      INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, market_type, scope_id, rotator_type)
);

ALTER TABLE wire_market_rotator ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_market_rotator TO service_role;

CREATE OR REPLACE FUNCTION advance_market_rotator(
  p_node_id UUID, p_market_type TEXT, p_scope_id TEXT, p_rotator_type TEXT
) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE v_pos INTEGER;
BEGIN
  INSERT INTO wire_market_rotator (node_id, market_type, scope_id, rotator_type, position)
    VALUES (p_node_id, p_market_type, p_scope_id, p_rotator_type, 1)
    ON CONFLICT (node_id, market_type, scope_id, rotator_type)
    DO UPDATE SET position = (wire_market_rotator.position % 80) + 1
    RETURNING position INTO v_pos;
  RETURN v_pos;
END;
$$;

-- Reads slot counts from economic_parameter contribution (Pillar 37).
-- Seed value 76/2/2 via market_rotator_config contribution.
CREATE OR REPLACE FUNCTION market_rotator_recipient(
  p_position INTEGER
) RETURNS TEXT
LANGUAGE plpgsql STABLE AS $$
DECLARE
  v_total INTEGER;
  v_wire INTEGER;
  v_gf INTEGER;
BEGIN
  SELECT
    COALESCE((c.structured_data->>'total_slots')::INTEGER, 80),
    COALESCE((c.structured_data->>'wire_slots')::INTEGER, 2),
    COALESCE((c.structured_data->>'graph_fund_slots')::INTEGER, 2)
  INTO v_total, v_wire, v_gf
  FROM wire_contributions c
  WHERE c.type = 'economic_parameter'
    AND c.structured_data->>'parameter_name' = 'market_rotator_config'
    AND c.status = 'active'
  ORDER BY c.created_at DESC LIMIT 1;

  v_total := COALESCE(v_total, 80);
  v_wire := COALESCE(v_wire, 2);
  v_gf := COALESCE(v_gf, 2);

  -- Bjorklund even distribution
  IF v_wire > 0 AND (p_position % (v_total / v_wire)) = 0 THEN
    RETURN 'wire';
  END IF;
  IF v_gf > 0 AND ((p_position + v_total / (v_gf * 2)) % (v_total / v_gf)) = 0 THEN
    RETURN 'graph_fund';
  END IF;
  RETURN 'provider';
END;
$$;

GRANT EXECUTE ON FUNCTION advance_market_rotator TO service_role;
GRANT EXECUTE ON FUNCTION market_rotator_recipient TO service_role;
```

**4. Create Wire platform operator entity:**

Register handle `agentwireplatform` (operator-level, Wire-owned). Balance may go negative. Replenished from platform revenue. Used ONLY for `compute_estimation_subsidy` debits.

### New Tables

```sql
-- Provider standing offers on the exchange.
-- UNIQUE constraint includes provider_type so same model can be offered as both local and bridge.
CREATE TABLE wire_compute_offers (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  node_id               UUID NOT NULL REFERENCES wire_nodes(id),
  operator_id           UUID NOT NULL REFERENCES wire_operators(id),
  model_id              TEXT NOT NULL,
  provider_type         TEXT NOT NULL DEFAULT 'local',  -- 'local' | 'bridge'
  rate_per_m_input      INTEGER NOT NULL,   -- credits per million input tokens (i64, Pillar 9)
  rate_per_m_output     INTEGER NOT NULL,   -- credits per million output tokens (i64, Pillar 9)
  reservation_fee       INTEGER NOT NULL DEFAULT 0,
  queue_discount_curve  JSONB NOT NULL DEFAULT '[]'::jsonb,  -- [{depth, multiplier_bps}]
  max_queue_depth       INTEGER NOT NULL DEFAULT 20,
  current_queue_depth   INTEGER NOT NULL DEFAULT 0,
  status                TEXT NOT NULL DEFAULT 'active',  -- 'active'|'inactive'|'offline'
  observed_median_tps   REAL,
  observed_p95_latency_ms INTEGER,
  observed_job_count    INTEGER DEFAULT 0,
  context_window        INTEGER,
  privacy_capabilities  TEXT[] DEFAULT '{standard}',  -- 'standard' | 'cloud_relay' | future: 'clean_room', 'vault'
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(node_id, model_id, provider_type)
);

ALTER TABLE wire_compute_offers ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_offers TO service_role;

CREATE INDEX idx_compute_offers_model ON wire_compute_offers(model_id) WHERE status = 'active';
CREATE INDEX idx_compute_offers_node ON wire_compute_offers(node_id);
```

```sql
-- Individual compute jobs (the atomic unit).
-- NO prompt/payload columns. The Wire NEVER has the prompt (relay-first model).
-- NO result_content column. Result delivered transiently via relay chain, then discarded.
CREATE TABLE wire_compute_jobs (
  id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  requester_node_id       UUID REFERENCES wire_nodes(id),
  requester_operator_id   UUID NOT NULL REFERENCES wire_operators(id),
  provider_node_id        UUID NOT NULL REFERENCES wire_nodes(id),
  provider_operator_id    UUID NOT NULL REFERENCES wire_operators(id),
  offer_id                UUID NOT NULL REFERENCES wire_compute_offers(id),
  model_id                TEXT NOT NULL,
  matched_rate_in_per_m   INTEGER NOT NULL,
  matched_rate_out_per_m  INTEGER NOT NULL,
  matched_queue_depth     INTEGER NOT NULL,
  matched_multiplier_bps  INTEGER NOT NULL,  -- basis points (10000 = 1.0x, 8500 = 0.85x)
  reservation_fee         INTEGER NOT NULL DEFAULT 0,
  status                  TEXT NOT NULL DEFAULT 'reserved',
    -- 'reserved'  -> slot in queue, maybe empty
    -- 'filled'    -> prompt submitted, deposit charged
    -- 'executing' -> GPU processing
    -- 'completed' -> result delivered, settled
    -- 'failed'    -> provider timeout/error
    -- 'cancelled' -> requester cancelled before execution
    -- 'void'      -> unfilled slot resolved as no-op
  relay_count             INTEGER NOT NULL DEFAULT 0,
  input_token_estimate    INTEGER,
  temperature             REAL,
  max_tokens              INTEGER,
  deposit_amount          INTEGER,
  actual_cost             INTEGER,
  graph_fund_levy         INTEGER,
  provider_payout         INTEGER,
  requester_refund        INTEGER,
  graph_fund_slot         BOOLEAN NOT NULL DEFAULT false,
  result_prompt_tokens    INTEGER,
  result_completion_tokens INTEGER,
  result_latency_ms       INTEGER,
  result_finish_reason    TEXT,
  created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
  filled_at               TIMESTAMPTZ,
  dispatched_at           TIMESTAMPTZ,
  completed_at            TIMESTAMPTZ,
  timeout_at              TIMESTAMPTZ,
  batch_id                UUID,
  queue_position          INTEGER
);

CREATE INDEX idx_compute_jobs_provider ON wire_compute_jobs(provider_node_id, status);
CREATE INDEX idx_compute_jobs_requester ON wire_compute_jobs(requester_operator_id, status);
CREATE INDEX idx_compute_jobs_batch ON wire_compute_jobs(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX idx_compute_jobs_timeout ON wire_compute_jobs(timeout_at) WHERE status IN ('executing', 'filled');

ALTER TABLE wire_compute_jobs ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_jobs TO service_role;
```

```sql
-- Performance observations (append-only).
CREATE TABLE wire_compute_observations (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id          UUID NOT NULL REFERENCES wire_compute_jobs(id),
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  model_id        TEXT NOT NULL,
  input_tokens    INTEGER NOT NULL,
  output_tokens   INTEGER NOT NULL,
  latency_ms      INTEGER NOT NULL,
  tokens_per_sec  REAL NOT NULL,
  time_to_first_token_ms INTEGER,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_compute_obs_node_model ON wire_compute_observations(node_id, model_id);
CREATE INDEX idx_compute_obs_model ON wire_compute_observations(model_id, created_at DESC);

ALTER TABLE wire_compute_observations ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_observations TO service_role;
```

```sql
-- Queue state mirror: per-model queue state per node.
-- Composite PK (node_id, model_id) -- NOT one row per node.
CREATE TABLE wire_compute_queue_state (
  node_id              UUID NOT NULL REFERENCES wire_nodes(id),
  model_id             TEXT NOT NULL,
  seq                  BIGINT NOT NULL DEFAULT 0,
  total_depth          INTEGER NOT NULL DEFAULT 0,
  market_depth         INTEGER NOT NULL DEFAULT 0,
  is_executing         BOOLEAN NOT NULL DEFAULT false,
  est_next_available_s INTEGER,
  max_market_depth     INTEGER NOT NULL DEFAULT 5,
  max_total_depth      INTEGER NOT NULL DEFAULT 20,
  updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (node_id, model_id)
);

ALTER TABLE wire_compute_queue_state ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_queue_state TO service_role;
```

```sql
-- Universal incentive pools (shared across compute, storage, relay markets).
CREATE TABLE wire_incentive_pools (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  funder_operator_id    UUID NOT NULL REFERENCES wire_operators(id),
  criteria_type         TEXT NOT NULL,
  criteria_params       JSONB NOT NULL,
  amount_remaining      INTEGER NOT NULL,
  payout_interval_s     INTEGER NOT NULL,
  rotator_position      INTEGER NOT NULL DEFAULT 0,
  status                TEXT NOT NULL DEFAULT 'active',  -- 'active' | 'exhausted' | 'cancelled'
  last_payout_at        TIMESTAMPTZ,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_incentive_pools ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_incentive_pools TO service_role;
CREATE INDEX idx_incentive_pools_criteria ON wire_incentive_pools(criteria_type) WHERE status = 'active';
CREATE INDEX idx_incentive_pools_payout ON wire_incentive_pools(last_payout_at) WHERE status = 'active';
```

---

## IX. Wire-Side RPCs

### `match_compute_job` -- Match requester to provider

**Audit fix applied:** Self-dealing check (`requester_operator_id != provider_operator_id`).

```sql
CREATE OR REPLACE FUNCTION match_compute_job(
  p_requester_operator_id UUID,
  p_requester_node_id UUID,
  p_model_id TEXT,
  p_max_budget INTEGER,
  p_input_tokens INTEGER,
  p_latency_preference TEXT DEFAULT 'best_price'
) RETURNS TABLE(
  job_id UUID,
  matched_rate_in INTEGER,
  matched_rate_out INTEGER,
  matched_multiplier_bps INTEGER,
  reservation_fee INTEGER,
  estimated_deposit INTEGER,
  queue_position INTEGER
)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_offer wire_compute_offers%ROWTYPE;
  v_queue wire_compute_queue_state%ROWTYPE;
  v_multiplier_bps INTEGER;
  v_est_output INTEGER;
  v_est_cost INTEGER;
  v_reservation INTEGER;
  v_total_est INTEGER;
  v_job_id UUID;
  v_new_depth INTEGER;
  v_res_rotator INTEGER;
  v_res_recipient TEXT;
  v_wire_platform_operator_id UUID;
  v_default_output_estimate INTEGER;
BEGIN
  -- Resolve Wire platform operator ONCE
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  -- Read default output estimate from economic_parameter (Pillar 37: no hardcoded 500)
  SELECT COALESCE((c.structured_data->>'default_tokens')::INTEGER, 500)
    INTO v_default_output_estimate
    FROM wire_contributions c
    WHERE c.type = 'economic_parameter'
      AND c.structured_data->>'parameter_name' = 'default_output_estimate_tokens'
      AND c.status = 'active'
    ORDER BY c.created_at DESC LIMIT 1;
  v_default_output_estimate := COALESCE(v_default_output_estimate, 500);

  -- Network median output for this model (from observations)
  SELECT COALESCE(
    percentile_cont(0.5) WITHIN GROUP (ORDER BY output_tokens),
    v_default_output_estimate
  )::INTEGER
    INTO v_est_output
    FROM wire_compute_observations
    WHERE model_id = p_model_id
      AND created_at > now() - interval '7 days';

  -- Find best matching offer
  -- AUDIT FIX: self-dealing check (requester_operator_id != provider operator_id)
  SELECT o.* INTO v_offer
    FROM wire_compute_offers o
    JOIN wire_compute_queue_state q ON q.node_id = o.node_id AND q.model_id = o.model_id
    WHERE o.model_id = p_model_id
      AND o.status = 'active'
      AND o.operator_id != p_requester_operator_id  -- AUDIT FIX: prevent self-dealing
      AND q.total_depth < o.max_queue_depth
      AND q.market_depth < q.max_market_depth
      AND q.updated_at > now() - interval '2 minutes'
    ORDER BY
      CASE p_latency_preference
        WHEN 'immediate' THEN q.total_depth
        WHEN 'best_price' THEN -q.total_depth
        ELSE q.total_depth
      END
    LIMIT 1
    FOR UPDATE OF o;

  IF NOT FOUND THEN
    RAISE EXCEPTION 'No matching provider available for model %', p_model_id;
  END IF;

  -- Lock queue state row separately
  SELECT * INTO v_queue FROM wire_compute_queue_state
    WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id
    FOR UPDATE;

  v_new_depth := v_queue.total_depth + 1;
  v_multiplier_bps := compute_queue_multiplier_bps(v_offer.queue_discount_curve, v_queue.total_depth);

  v_reservation := v_offer.reservation_fee;
  v_est_cost := CEIL(p_input_tokens::NUMERIC * v_offer.rate_per_m_input * v_multiplier_bps / (1000000::NUMERIC * 10000))
              + CEIL(v_est_output::NUMERIC * v_offer.rate_per_m_output * v_multiplier_bps / (1000000::NUMERIC * 10000));
  v_total_est := v_reservation + v_est_cost;

  IF v_total_est > p_max_budget THEN
    RAISE EXCEPTION 'Estimated cost (%) exceeds budget (%)', v_total_est, p_max_budget;
  END IF;

  INSERT INTO wire_compute_jobs (
    requester_node_id, requester_operator_id,
    provider_node_id, provider_operator_id, offer_id,
    model_id, matched_rate_in_per_m, matched_rate_out_per_m,
    matched_queue_depth, matched_multiplier_bps,
    reservation_fee, status, queue_position, timeout_at
  ) VALUES (
    p_requester_node_id, p_requester_operator_id,
    v_offer.node_id, v_offer.operator_id, v_offer.id,
    p_model_id, v_offer.rate_per_m_input, v_offer.rate_per_m_output,
    v_new_depth, v_multiplier_bps,
    v_reservation, 'reserved', v_new_depth,
    now() + (COALESCE(v_queue.est_next_available_s, 120) * v_new_depth || ' seconds')::interval
  ) RETURNING id INTO v_job_id;

  PERFORM debit_operator_atomic(p_requester_operator_id, v_reservation,
    'compute_reservation', v_job_id, 'compute_market');

  v_res_rotator := advance_market_rotator(v_offer.node_id, 'compute', v_offer.model_id, 'reservation');
  v_res_recipient := market_rotator_recipient(v_res_rotator);

  IF v_res_recipient = 'graph_fund' THEN
    INSERT INTO wire_graph_fund (amount, source_type, reference_id)
      VALUES (v_reservation, 'compute_reservation', v_job_id);
  ELSIF v_res_recipient = 'wire' THEN
    PERFORM credit_operator_atomic(v_wire_platform_operator_id, v_reservation,
      'compute_reservation_wire_take', v_job_id, 'compute_market');
  ELSE
    PERFORM credit_operator_atomic(v_offer.operator_id, v_reservation,
      'compute_reservation_income', v_job_id, 'compute_market');
  END IF;

  UPDATE wire_compute_queue_state SET
    market_depth = market_depth + 1,
    total_depth = total_depth + 1,
    updated_at = now()
  WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id;  -- AUDIT FIX: model_id filter

  UPDATE wire_compute_offers SET
    current_queue_depth = v_new_depth,
    updated_at = now()
  WHERE id = v_offer.id;

  RETURN QUERY SELECT v_job_id,
    v_offer.rate_per_m_input, v_offer.rate_per_m_output,
    v_multiplier_bps, v_reservation, v_est_cost, v_new_depth;
END;
$$;

GRANT EXECUTE ON FUNCTION match_compute_job TO service_role;
```

### `fill_compute_job` -- Fill reserved slot with token count

**Audit fixes applied:** `v_wire_platform_operator_id` DECLARED and resolved. `select_relay_chain` called ONCE and stored. NO prompts accepted.

```sql
CREATE OR REPLACE FUNCTION fill_compute_job(
  p_job_id UUID,
  p_requester_operator_id UUID,
  p_input_token_count INTEGER,
  p_relay_count INTEGER DEFAULT 0
) RETURNS TABLE(
  deposit_charged INTEGER,
  relay_chain JSONB,
  provider_ephemeral_pubkey TEXT,
  total_relay_fee INTEGER
) AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
  v_est_output INTEGER;
  v_deposit INTEGER;
  v_relay_fee INTEGER := 0;
  v_relay_info JSONB := '[]'::jsonb;
  v_provider_pubkey TEXT;
  v_relay RECORD;
  v_wire_platform_operator_id UUID;  -- AUDIT FIX: declared
  v_default_output_estimate INTEGER;
BEGIN
  -- AUDIT FIX: resolve Wire platform operator ONCE
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'reserved'
      AND requester_operator_id = p_requester_operator_id
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in reserved status';
  END IF;

  -- Read default output estimate from economic_parameter (Pillar 37)
  SELECT COALESCE((c.structured_data->>'default_tokens')::INTEGER, 500)
    INTO v_default_output_estimate
    FROM wire_contributions c
    WHERE c.type = 'economic_parameter'
      AND c.structured_data->>'parameter_name' = 'default_output_estimate_tokens'
      AND c.status = 'active'
    ORDER BY c.created_at DESC LIMIT 1;
  v_default_output_estimate := COALESCE(v_default_output_estimate, 500);

  SELECT COALESCE(
    percentile_cont(0.5) WITHIN GROUP (ORDER BY output_tokens),
    v_default_output_estimate
  )::INTEGER
    INTO v_est_output
    FROM wire_compute_observations
    WHERE model_id = v_job.model_id AND created_at > now() - interval '7 days';

  v_deposit := CEIL(p_input_token_count::NUMERIC * v_job.matched_rate_in_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000))
             + CEIL(v_est_output::NUMERIC * v_job.matched_rate_out_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000));

  -- AUDIT FIX: select_relay_chain called ONCE and stored
  IF p_relay_count > 0 THEN
    -- Stub: reject relay_count > 0 until relay market ships
    RAISE EXCEPTION 'Relay routing not yet available (relay_count must be 0 at launch)';
  END IF;

  PERFORM debit_operator_atomic(p_requester_operator_id, v_deposit,
    'compute_deposit', p_job_id, 'compute_market');

  v_provider_pubkey := encode(gen_random_bytes(32), 'hex');  -- placeholder: real X25519

  -- NO prompts accepted -- Wire never has the prompt (AUDIT FIX)
  UPDATE wire_compute_jobs SET
    status = 'filled',
    input_token_estimate = p_input_token_count,
    relay_count = p_relay_count,
    deposit_amount = v_deposit,
    filled_at = now()
  WHERE id = p_job_id;

  RETURN QUERY SELECT v_deposit, COALESCE(v_relay_info, '[]'::jsonb), v_provider_pubkey, v_relay_fee;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

GRANT EXECUTE ON FUNCTION fill_compute_job TO service_role;
```

### `start_compute_job` -- Transition filled to executing

**NEW RPC -- was missing from original plan (audit finding).**

```sql
CREATE OR REPLACE FUNCTION start_compute_job(
  p_job_id UUID,
  p_provider_node_id UUID
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
BEGIN
  UPDATE wire_compute_jobs SET
    status = 'executing',
    dispatched_at = now()
  WHERE id = p_job_id
    AND status = 'filled'
    AND provider_node_id = p_provider_node_id;

  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job % not found or not in filled status for provider %', p_job_id, p_provider_node_id;
  END IF;
END;
$$;

GRANT EXECUTE ON FUNCTION start_compute_job TO service_role;
```

### `settle_compute_job` -- Provider reports completion

**Audit fixes applied:** model_id filter on queue decrement. Single operator resolution. Basis points not f64.

```sql
CREATE OR REPLACE FUNCTION settle_compute_job(
  p_job_id UUID,
  p_prompt_tokens INTEGER,
  p_completion_tokens INTEGER,
  p_latency_ms INTEGER,
  p_finish_reason TEXT
) RETURNS TABLE(actual_cost INTEGER, provider_payout INTEGER, requester_adjustment INTEGER)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
  v_actual_cost INTEGER;
  v_graph_fund INTEGER;
  v_wire_take INTEGER;
  v_provider_payout INTEGER;
  v_requester_adj INTEGER;
  v_wire_subsidy INTEGER;
  v_rotator_pos INTEGER;
  v_slot_recipient TEXT;
  v_wire_platform_operator_id UUID;
BEGIN
  -- Resolve Wire platform operator ONCE (AUDIT FIX: single resolution)
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'executing'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in executing status';
  END IF;

  IF v_job.max_tokens IS NOT NULL AND p_completion_tokens > v_job.max_tokens * 2 THEN
    RAISE EXCEPTION 'Reported completion_tokens (%) exceeds plausible limit', p_completion_tokens;
  END IF;

  -- ALL integer arithmetic (Pillar 9: basis points, not f64)
  v_actual_cost := CEIL(p_prompt_tokens::NUMERIC * v_job.matched_rate_in_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000))
                 + CEIL(p_completion_tokens::NUMERIC * v_job.matched_rate_out_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000));

  -- Rotator arm settlement
  v_rotator_pos := advance_market_rotator(v_job.provider_node_id, 'compute', v_job.model_id, 'settlement');
  v_slot_recipient := market_rotator_recipient(v_rotator_pos);

  IF v_slot_recipient = 'graph_fund' THEN
    v_graph_fund := v_actual_cost; v_wire_take := 0; v_provider_payout := 0;
  ELSIF v_slot_recipient = 'wire' THEN
    v_graph_fund := 0; v_wire_take := v_actual_cost; v_provider_payout := 0;
  ELSE
    v_graph_fund := 0; v_wire_take := 0; v_provider_payout := v_actual_cost;
  END IF;

  -- Deposit reconciliation: requester NEVER pays more than the estimate
  v_requester_adj := COALESCE(v_job.deposit_amount, 0) - v_actual_cost;

  IF v_requester_adj < 0 THEN
    v_wire_subsidy := ABS(v_requester_adj);
    v_requester_adj := 0;
    IF v_wire_platform_operator_id IS NOT NULL THEN
      UPDATE wire_operators SET credit_balance = credit_balance - v_wire_subsidy
        WHERE id = v_wire_platform_operator_id;
      INSERT INTO wire_credits_ledger (operator_id, amount, reason, reference_id, category, balance_after)
        VALUES (v_wire_platform_operator_id, -v_wire_subsidy, 'compute_estimation_subsidy', p_job_id, 'compute_market',
                (SELECT credit_balance FROM wire_operators WHERE id = v_wire_platform_operator_id));
    END IF;
  END IF;

  UPDATE wire_compute_jobs SET
    status = 'completed',
    result_prompt_tokens = p_prompt_tokens,
    result_completion_tokens = p_completion_tokens,
    result_latency_ms = p_latency_ms,
    result_finish_reason = p_finish_reason,
    actual_cost = v_actual_cost,
    graph_fund_levy = v_graph_fund,
    provider_payout = v_provider_payout,
    requester_refund = v_requester_adj,
    graph_fund_slot = (v_slot_recipient != 'provider'),
    completed_at = now()
  WHERE id = p_job_id;

  IF v_provider_payout > 0 THEN
    PERFORM credit_operator_atomic(v_job.provider_operator_id, v_provider_payout,
      'compute_serve', p_job_id, 'compute_market');
  END IF;
  IF v_wire_take > 0 THEN
    PERFORM credit_operator_atomic(v_wire_platform_operator_id, v_wire_take,
      'compute_wire_take', p_job_id, 'compute_market');
  END IF;
  IF v_graph_fund > 0 THEN
    INSERT INTO wire_graph_fund (amount, source_type, reference_id)
      VALUES (v_graph_fund, 'compute_service', p_job_id);
  END IF;

  IF v_requester_adj > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_requester_adj,
      'compute_refund', p_job_id, 'compute_market');
  END IF;

  INSERT INTO wire_compute_observations (job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec)
    VALUES (p_job_id, v_job.provider_node_id, v_job.model_id, p_prompt_tokens, p_completion_tokens, p_latency_ms,
            CASE WHEN p_latency_ms > 0 THEN p_completion_tokens::REAL / (p_latency_ms::REAL / 1000) ELSE 0 END);

  -- AUDIT FIX: model_id filter on queue decrement
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id AND model_id = v_job.model_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;

  RETURN QUERY SELECT v_actual_cost, v_provider_payout, v_requester_adj;
END;
$$;

GRANT EXECUTE ON FUNCTION settle_compute_job TO service_role;
```

### `fail_compute_job`

**Audit fix applied:** model_id filter on queue decrement.

```sql
CREATE OR REPLACE FUNCTION fail_compute_job(
  p_job_id UUID,
  p_reason TEXT DEFAULT 'timeout'
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
BEGIN
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status IN ('executing', 'filled')
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in executable status';
  END IF;

  IF COALESCE(v_job.deposit_amount, 0) > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_job.deposit_amount,
      'compute_fail_refund', p_job_id, 'compute_market');
  END IF;

  UPDATE wire_compute_jobs SET
    status = 'failed',
    result_finish_reason = p_reason,
    completed_at = now()
  WHERE id = p_job_id;

  -- AUDIT FIX: model_id filter
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id AND model_id = v_job.model_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;

  INSERT INTO wire_compute_observations (job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec)
    VALUES (p_job_id, v_job.provider_node_id, v_job.model_id, 0, 0, 0, 0);
END;
$$;

GRANT EXECUTE ON FUNCTION fail_compute_job TO service_role;
```

### `void_compute_job`

**Audit fix applied:** model_id filter on queue decrement.

```sql
CREATE OR REPLACE FUNCTION void_compute_job(
  p_job_id UUID
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
BEGIN
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'reserved'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in reserved status';
  END IF;

  UPDATE wire_compute_jobs SET status = 'void', completed_at = now()
    WHERE id = p_job_id;

  -- AUDIT FIX: model_id filter
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

GRANT EXECUTE ON FUNCTION void_compute_job TO service_role;
```

### `cancel_compute_job` -- NEW (was missing from original plan)

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
      AND status IN ('reserved', 'filled')
      AND requester_operator_id = p_requester_operator_id
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found, not cancellable, or not owned by requester';
  END IF;

  -- Refund deposit if filled (deposit was charged). Reservation fee is NOT refunded.
  IF v_job.status = 'filled' AND COALESCE(v_job.deposit_amount, 0) > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_job.deposit_amount,
      'compute_cancel_refund', p_job_id, 'compute_market');
  END IF;

  UPDATE wire_compute_jobs SET
    status = 'cancelled',
    completed_at = now()
  WHERE id = p_job_id;

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

### `clawback_compute_job` -- NEW (needed for Phase 5 quality enforcement)

```sql
CREATE OR REPLACE FUNCTION clawback_compute_job(
  p_job_id UUID,
  p_challenger_operator_id UUID,
  p_challenge_stake INTEGER
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_job wire_compute_jobs%ROWTYPE;
  v_clawback_amount INTEGER;
  v_bounty INTEGER;
  v_wire_platform_operator_id UUID;
BEGIN
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'completed'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in completed status';
  END IF;

  -- Clawback provider_payout (may create negative balance for provider)
  v_clawback_amount := COALESCE(v_job.provider_payout, 0);
  IF v_clawback_amount > 0 THEN
    -- Debit from provider (balance may go negative -- tracked, recovered over time)
    UPDATE wire_operators SET credit_balance = credit_balance - v_clawback_amount
      WHERE id = v_job.provider_operator_id;
    INSERT INTO wire_credits_ledger (operator_id, amount, reason, reference_id, category, balance_after)
      VALUES (v_job.provider_operator_id, -v_clawback_amount, 'compute_clawback', p_job_id, 'compute_market',
              (SELECT credit_balance FROM wire_operators WHERE id = v_job.provider_operator_id));
  END IF;

  -- Refund requester (original deposit minus what they already got back)
  PERFORM credit_operator_atomic(v_job.requester_operator_id,
    COALESCE(v_job.actual_cost, 0),
    'compute_clawback_refund', p_job_id, 'compute_market');

  -- Challenger bounty: proportional to job cost (funded by clawback)
  v_bounty := GREATEST(v_clawback_amount / 10, 1);
  PERFORM credit_operator_atomic(p_challenger_operator_id, v_bounty,
    'challenge_bounty', p_job_id, 'compute_market');

  -- Return challenger's stake
  PERFORM credit_operator_atomic(p_challenger_operator_id, p_challenge_stake,
    'challenge_stake_return', p_job_id, 'compute_market');
END;
$$;

GRANT EXECUTE ON FUNCTION clawback_compute_job TO service_role;
```

### `sweep_timed_out_compute_jobs`

```sql
CREATE OR REPLACE FUNCTION sweep_timed_out_compute_jobs()
RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_count INTEGER := 0;
  v_job RECORD;
BEGIN
  FOR v_job IN
    SELECT id FROM wire_compute_jobs
    WHERE status IN ('executing', 'filled')
      AND timeout_at < now()
    FOR UPDATE SKIP LOCKED
    LIMIT 100
  LOOP
    PERFORM fail_compute_job(v_job.id, 'timeout');
    v_count := v_count + 1;
  END LOOP;
  RETURN v_count;
END;
$$;

GRANT EXECUTE ON FUNCTION sweep_timed_out_compute_jobs TO service_role;
```

### `deactivate_stale_compute_offers`

**Audit fix applied:** reads threshold from economic_parameter, not hardcoded.

```sql
CREATE OR REPLACE FUNCTION deactivate_stale_compute_offers()
RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_count INTEGER;
  v_threshold_minutes INTEGER;
BEGIN
  -- Read stale threshold from economic_parameter contribution (Pillar 37)
  SELECT COALESCE((c.structured_data->>'threshold_minutes')::INTEGER, 5)
    INTO v_threshold_minutes
    FROM wire_contributions c
    WHERE c.type = 'economic_parameter'
      AND c.structured_data->>'parameter_name' = 'stale_offer_threshold_minutes'
      AND c.status = 'active'
    ORDER BY c.created_at DESC LIMIT 1;
  v_threshold_minutes := COALESCE(v_threshold_minutes, 5);

  UPDATE wire_compute_offers SET status = 'offline', updated_at = now()
  WHERE status = 'active'
    AND node_id IN (
      SELECT id FROM wire_nodes
      WHERE last_seen_at < now() - (v_threshold_minutes || ' minutes')::interval
    );
  GET DIAGNOSTICS v_count = ROW_COUNT;
  RETURN v_count;
END;
$$;

GRANT EXECUTE ON FUNCTION deactivate_stale_compute_offers TO service_role;
```

### `aggregate_compute_observations` -- NEW (needed before Phase 5)

```sql
CREATE OR REPLACE FUNCTION aggregate_compute_observations(
  p_node_id UUID,
  p_model_id TEXT
) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_median_tps REAL;
  v_p95_latency INTEGER;
  v_job_count INTEGER;
BEGIN
  SELECT
    percentile_cont(0.5) WITHIN GROUP (ORDER BY tokens_per_sec),
    percentile_cont(0.95) WITHIN GROUP (ORDER BY latency_ms)::INTEGER,
    COUNT(*)
  INTO v_median_tps, v_p95_latency, v_job_count
  FROM wire_compute_observations
  WHERE node_id = p_node_id
    AND model_id = p_model_id
    AND created_at > now() - interval '7 days';

  UPDATE wire_compute_offers SET
    observed_median_tps = v_median_tps,
    observed_p95_latency_ms = v_p95_latency,
    observed_job_count = v_job_count,
    updated_at = now()
  WHERE node_id = p_node_id AND model_id = p_model_id AND status = 'active';
END;
$$;

GRANT EXECUTE ON FUNCTION aggregate_compute_observations TO service_role;
```

### Helper: `compute_queue_multiplier_bps`

```sql
CREATE OR REPLACE FUNCTION compute_queue_multiplier_bps(
  p_curve JSONB,
  p_depth INTEGER
) RETURNS INTEGER AS $$
DECLARE
  v_prev_depth INTEGER := 0;
  v_prev_bps INTEGER := 10000;
  v_next_depth INTEGER;
  v_next_bps INTEGER;
  v_entry JSONB;
BEGIN
  IF p_curve IS NULL OR jsonb_array_length(p_curve) = 0 THEN
    RETURN 10000;
  END IF;

  FOR v_entry IN SELECT * FROM jsonb_array_elements(p_curve) ORDER BY (value->>'depth')::INTEGER
  LOOP
    v_next_depth := (v_entry->>'depth')::INTEGER;
    v_next_bps := (v_entry->>'multiplier_bps')::INTEGER;

    IF p_depth <= v_next_depth THEN
      IF v_next_depth = v_prev_depth THEN RETURN v_next_bps; END IF;
      RETURN v_prev_bps + (v_next_bps - v_prev_bps) *
             (p_depth - v_prev_depth) / (v_next_depth - v_prev_depth);
    END IF;

    v_prev_depth := v_next_depth;
    v_prev_bps := v_next_bps;
  END LOOP;

  RETURN v_prev_bps;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

GRANT EXECUTE ON FUNCTION compute_queue_multiplier_bps TO service_role;
```

---

## X. Wire-Side API Routes

### Exchange Endpoints

| Method | Path | Auth | RPC Called | Description |
|--------|------|------|-----------|-------------|
| POST | `/api/v1/compute/match` | Node JWT | `match_compute_job` | Match job, create reserved slot |
| POST | `/api/v1/compute/fill` | Node JWT | `fill_compute_job` | Fill reserved slot with token count |
| POST | `/api/v1/compute/start` | Node JWT | `start_compute_job` | Provider confirms GPU start |
| POST | `/api/v1/compute/cancel` | Node JWT | `cancel_compute_job` | Cancel reserved/filled job |
| POST | `/api/v1/compute/settle` | Node JWT | `settle_compute_job` | Provider reports completion |
| POST | `/api/v1/compute/void` | Node JWT | `void_compute_job` | Unfilled slot reached front |
| POST | `/api/v1/compute/fail` | Node JWT | `fail_compute_job` | Provider reports failure |
| GET | `/api/v1/compute/market-surface` | Public | Direct query | Aggregated market view |
| GET | `/api/v1/compute/job/:id` | Node JWT | Direct query | Job status |
| POST | `/api/v1/compute/reserve-batch` | Node JWT | N x `match_compute_job` | Speculative batch reservation |

### Provider Management Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/v1/compute/offers` | Node JWT | Create/update standing offer |
| DELETE | `/api/v1/compute/offers/:id` | Node JWT | Withdraw an offer |
| GET | `/api/v1/compute/offers/mine` | Node JWT | List my active offers |
| POST | `/api/v1/compute/queue-state` | Node JWT | Push queue state snapshot |
| GET | `/api/v1/compute/performance/:nodeId/:modelId` | Node JWT | Network-observed performance |

---

## XI. Contribution Types

All new contribution types follow Law 3 (one contribution store):

| Schema Type | Purpose | Where | Example |
|---|---|---|---|
| `compute_pricing` | Per-model pricing with optional competitive strategy | Node config | See below |
| `compute_capacity` | Max market depth, max total depth, concurrency | Node config | See below |
| `compute_bridge` | Bridge configuration (models, API key reference, margin) | Node config | See below |
| `sentinel_check` | Sentinel check chain YAML | Node config | -- |
| `steward_methodology` | Steward experiment chain YAML | Node config | -- |
| `incentive_pool` | Universal incentive pool (any criteria type) | Wire contributions | See below |
| `incentive_criteria` | Criteria type definition for incentive pools | Wire contributions | -- |
| `economic_parameter` | System parameters (deposit %, thresholds, rotator config) | Wire contributions | See below |

### YAML Examples

**`compute_pricing`:**

```yaml
schema_type: compute_pricing
model: llama-3.1-70b-instruct
pricing_mode: competitive        # "fixed" | "competitive"
competitive_target: match_best   # "match_best" | "undercut_best" | "premium_over_best"
competitive_offset_bps: 0
floor_per_m_input: 200
floor_per_m_output: 300
ceiling_per_m_input: 2000
ceiling_per_m_output: 3000
rate_per_m_input: 500            # used when pricing_mode = "fixed"
rate_per_m_output: 800
reservation_fee: 2
queue_discount_curve:
  - depth: 0
    multiplier_bps: 10000        # 1.0x
  - depth: 3
    multiplier_bps: 8500         # 0.85x
  - depth: 8
    multiplier_bps: 6500         # 0.65x
  - depth: 15
    multiplier_bps: 4500         # 0.45x
max_queue_depth: 20
```

**`compute_capacity`:**

```yaml
schema_type: compute_capacity
model: llama-3.1-70b-instruct
max_market_depth: 10
max_total_depth: 20
gpu_concurrency: 1
```

**`compute_bridge`:**

```yaml
schema_type: compute_bridge
models:
  - model_id: claude-3.5-sonnet
    openrouter_model: anthropic/claude-3.5-sonnet
    margin_target_bps: 1500      # 15% margin over OpenRouter cost
    max_concurrent: 5
  - model_id: gpt-4o
    openrouter_model: openai/gpt-4o
    margin_target_bps: 1000
    max_concurrent: 3
api_key_name: bridge_dedicated   # must be separate from personal use (DD-11)
privacy_indicator: cloud_relay   # NOT 'standard' -- bridge degrades privacy
refresh_interval_s: 3600         # check OpenRouter model availability hourly
error_suspension:
  on_402: suspend_all            # insufficient funds -> suspend all bridge offers
  on_429: backoff_exponential
  on_503: retry_3x_then_fail
```

**`incentive_pool`:**

```yaml
schema_type: incentive_pool
pool_name: "Keep llama-70b available on 5+ nodes"
criteria_type: model_availability
criteria_params:
  model_id: llama-3.1-70b-instruct
  min_providers: 5
amount_remaining: 10000
payout_interval_s: 3600
status: active
```

**`economic_parameter` (various):**

```yaml
# Rotator arm slot configuration
schema_type: economic_parameter
parameter_name: market_rotator_config
total_slots: 80
wire_slots: 2
graph_fund_slots: 2

---

# Default output token estimate for cold-start models
schema_type: economic_parameter
parameter_name: default_output_estimate_tokens
default_tokens: 500

---

# Stale offer deactivation threshold
schema_type: economic_parameter
parameter_name: stale_offer_threshold_minutes
threshold_minutes: 5

---

# Deposit percentage (100% = full prepayment of estimate)
schema_type: economic_parameter
parameter_name: deposit_percentage
percentage_bps: 10000            # basis points: 10000 = 100%

---

# Queue mirror debounce window
schema_type: economic_parameter
parameter_name: queue_mirror_debounce_ms
debounce_ms: 500

---

# Challenge stake multiplier (stake = multiplier * job actual_cost)
schema_type: economic_parameter
parameter_name: challenge_stake_multiplier
multiplier_bps: 5000             # basis points: 5000 = 50% of job cost
```

---

## XII. Design Decisions

**DD-1: Graph Fund identity.** Settlement uses handle `agentwiregraphfund` -- resolved to agent_id at settlement time via standard handle resolution. Not a hardcoded UUID.

**DD-2: Quality enforcement.** Quality uses existing challenge infrastructure (Pillar 24) + steward quality publications + timing anomaly detection. No dedicated review market (review pricing creates a cheap-review attack vector).

**DD-3: Bridge margin.** Pure market pricing. No platform enforcement. Providers set whatever strategy they want. The market sorts it out.

**DD-4: Sentinel model.** Configurable per node (contribution-driven). No standard model mandated.

**DD-5: Steward publication.** Operator decides publication policy (contribution). Not auto-published.

**DD-6: Deposit percentage.** The ratio of estimated cost locked as deposit. Stored as `economic_parameter` contribution (`deposit_percentage`), supersedable as market matures.

**DD-7: Negative balances.** Allowed on settlement when actual > estimate for the Wire platform operator only. Bounded per job (delta between estimate and actual). Self-corrects as network estimates improve.

**DD-8: Unfilled reservation resolution.** Unfilled slots resolve instantly as no-ops when they reach queue front. No token deposit charged. Reservation fee stays with provider.

**DD-9: Economic gates, not rate limits.** Every market operation costs at least 1 credit. No zero-cost operations. Challenge filing requires stake proportional to job `actual_cost`. Rejected challenge forfeits stake to challenged provider (Pillar 11: the benefit of cheating funds the defense).

**DD-10: Steward-mediated operation.** The operator does not manually manage market participation. The steward acts autonomously within the operator's experimental territory.

**DD-11: OpenRouter dual-use.** Personal use and bridge mode are independent configurations. Both can be active simultaneously. Bridge MUST use a dedicated API key (separate from personal use) to prevent bridge traffic from exhausting rate limits on personal builds.

---

## XIII. Pillar Conformance

| Pillar | How Respected |
|---|---|
| 1 (Everything is a contribution) | Pricing, capacity, bridge config, sentinel chains, steward methodology -- all contributions. Jobs are records, not contributions. |
| 2 (All the way down) | Pricing curves, discount functions, deposit percentages -- all contributable and supersedable. |
| 3 (Strict derived_from) | Compute jobs are service payments, not contributions with derivation chains. |
| 5 (Immutability + supersession) | Pricing changes supersede. Config changes supersede. No mutation of existing contributions. |
| 7 (UFF) | No creator/source-chain split. Wire 2.5% + Graph Fund 2.5% via rotator arm (76/2/2 out of 80 slots). Provider receives 95%. |
| 9 (Integer economics) | All credit amounts are integers. Rates are per-million-tokens. CEIL on cost calculations. Queue discount multipliers stored as integer basis points (8500 = 0.85x). Rust structs use `i64` not `f64`. Rotator arm uses integer slot positions. |
| 12 (Emergent pricing) | The order book IS emergent pricing. No Wire-set prices. |
| 14 (Handle-paths) | Graph Fund addressed by handle `agentwiregraphfund`. Wire platform by `agentwireplatform`. |
| 18 (One IR, one executor) | Market-served jobs run through `call_model_unified_with_audit_and_ctx` -- same path as local builds. |
| 21 (Dual-gate) | Providers must meet economic AND reputation thresholds to publish offers. Enforced at offer creation endpoint. |
| 23 (Preview-then-commit) | Market surface shows pricing, queue depth, ETA before requester commits. DADBEAR preview gate for cost estimation. |
| 24 (Challenge panels) | Quality disputes use existing challenge infrastructure. |
| 25 (Platform agents use public API) | Wire's own compute needs go through the same exchange. |
| 35 (Graph Fund) | 2.5% via rotator arm (2/80 slots) on both settlement and reservation fees. |
| 37 (Never prescribe outputs) | All thresholds, rates, limits are contribution-driven. Default output estimate from `economic_parameter`, not hardcoded 500. Stale offer threshold from `economic_parameter`, not hardcoded 5 minutes. |
| Law 1 (One executor) | All inference goes through the chain executor's LLM dispatch path. |
| Law 3 (One contribution store) | All config is contributions. No new tables for user-facing data. |
| Law 4 (StepContext) | Market-served jobs get StepContext with work_item_id and attempt_id for DADBEAR correlation. |

---

## XIV. Economic Parameters

Every `economic_parameter` contribution that must be seeded before the compute market can operate:

| Parameter Name | Purpose | Seed Value | Units |
|---|---|---|---|
| `market_rotator_config` | Rotator arm slot distribution | `{total_slots: 80, wire_slots: 2, graph_fund_slots: 2}` | Integer slot counts |
| `default_output_estimate_tokens` | Cold-start fallback for output token estimation | `{default_tokens: 500}` | Tokens |
| `stale_offer_threshold_minutes` | How long since last heartbeat before offer marked offline | `{threshold_minutes: 5}` | Minutes |
| `deposit_percentage` | Ratio of estimated cost locked as deposit | `{percentage_bps: 10000}` | Basis points (10000 = 100%) |
| `queue_mirror_debounce_ms` | Minimum interval between queue state pushes to Wire | `{debounce_ms: 500}` | Milliseconds |
| `challenge_stake_multiplier` | Stake required to file a challenge (as ratio of job cost) | `{multiplier_bps: 5000}` | Basis points (5000 = 50%) |
| `match_search_fee` | Credit cost per match attempt (refunded on success) | `{fee: 1}` | Credits |
| `offer_creation_fee` | Credit cost to create an offer (anti-spam) | `{fee: 1}` | Credits |
| `queue_push_fee` | Credit cost per queue state push (batched) | `{fee: 1}` | Credits |
| `relay_hop_fee` | Per-hop relay fee (for future relay market) | `{fee: 1}` | Credits |
| `fleet_jwt_ttl_secs` | TTL for fleet identity JWTs | `{ttl_secs: 3600}` | Seconds |
| `max_completion_token_ratio` | Guard: max reported completion_tokens as ratio of max_tokens | `{ratio: 2}` | Multiplier |
