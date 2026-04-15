# Wire Compute Market — Build Plan

**Date:** 2026-04-13
**Scope:** Full compute market implementation across agent-wire-node and GoodNewsEveryone. Order book exchange, queue mirroring, two-part pricing, bridge operations, review/quality, daemon intelligence, sentinel, and steward.
**Companion docs:** `GoodNewsEveryone/docs/architecture/wire-compute-market.md`, `wire-node-steward-daemon.md`, `wire-model-ecosystem.md`, `openrouter-settlement-layer.md`, `wire-market-privacy-tiers.md`

---

## I. Architecture Overview

The Wire Compute Market is a decentralized inference exchange where nodes buy and sell LLM compute using credits. The Wire acts as clearing house — matching bids and asks, escrowing payments, settling actual costs, and enforcing the Graph Fund levy. No dollars change hands on the Wire.

### Core Principles

1. **Order book, not centralized routing.** Providers publish standing offers (asks). Requesters submit jobs (bids). The exchange matches when bid ≥ ask. The Wire doesn't decide routing — the price does.

2. **Per-model FIFO queues.** Each loaded model on a node has its own independent queue. Local builds and market jobs share each model's queue. No priority classes, no preemption. Pricing IS priority. Nodes with multiple models loaded have multiple independent queues with independent depths, pricing curves, and throughput.

3. **Network-observed performance.** Nodes never self-report throughput. The network measures actual delivery times from completed jobs, segmented by model, input size, and output size. This data flows to both the node and the market.

4. **Individual calls as atomic unit.** Each queue operates on single LLM calls. Batches are composed of individual calls. Steps register when they're actually needed (pyramid builds are emergent).

5. **Serial GPU execution per model.** Default concurrency: 1 per model queue. Queue feeds one job at a time. Owner can override. Most stable, most predictable. Multiple model queues may run concurrently if hardware supports it.

6. **Two-part pricing through Wire escrow.** Reservation fee (fixed, non-refundable, to provider immediately) + per-token metered cost (estimated on fill, escrowed by Wire, settled against actual on completion).

7. **Queue mirror.** Queues exist locally regardless. When the node goes online for compute market, the Wire gets a mirror. Two sources add to each model's queue (local + market). Both sides see the same state.

8. **Push everywhere, pull nowhere.** All result delivery via webhook to tunnel URLs. The Wire pushes results to requesters. Providers push results to Wire. No polling. Every node has a tunnel.

9. **Fleet-first routing.** Nodes under the same operator (Wire account) route to each other directly — no Wire proxy, no credits, no settlement. Fleet traffic is completely private (bypasses the Wire). The exchange is only used when fleet capacity is exhausted.

10. **Zero hardcoded numbers.** Every parameter the exchange reads is a contribution: pricing, curves, levy rates, thresholds, timeouts, deposit percentages, matching weights. The exchange is a mechanism that reads contributions and applies them. Pillar 37 absolute.

### Credit Flow

All compute market credit flows go through the Wire as clearing house:

```
Requester → Wire (reservation fee + token deposit)
Wire → Provider (reservation fee immediately, actual token cost on settlement)
Wire → Graph Fund (2.5% of actual token cost)
Wire → Requester (refund if estimate > actual, or debit if actual > estimate)
```

Service payments, not contribution royalties. No creator/source-chain split (no UFF 60/35). But Wire 2.5% + Graph Fund 2.5% both apply — the Wire provides the exchange, proxy, escrow, and settlement infrastructure. Provider receives 95%. Implemented via rotator arm: 76 provider slots, 2 Wire slots, 2 Graph Fund slots out of 80.

### Privacy Model — Relay Network

**All compute market data flows node-to-node through the relay network. The Wire is pure control plane — it never sees payloads.** See `docs/architecture/wire-market-privacy-tiers.md` and `relay-market-plan.md` for full design.

The requester chooses how many relay hops to use (0 to N, contribution-driven privacy policy). The Wire sets up the relay chain and provides routing instructions. Data flows between nodes — the Wire never handles payloads.

**Provider sees:** The prompt (must — they run inference), model, parameters, job token.
**Provider does NOT see:** Requester identity (sees last relay's tunnel URL, which rotates), build context, pyramid slug, layer, step name, or any linking metadata.
**Wire sees:** Matching metadata, settlement data. NEVER sees prompt content or inference results.
**Fan-out policy:** Requester's dispatch policy includes `max_jobs_per_provider` (contribution-driven). Limits how many calls any single provider sees per build.

**Three privacy mechanisms (all launch, all orthogonal):**
1. **Variable relay count** (0-N hops, requester's choice) → topology ambiguity. Even 0-relay users get plausible deniability.
2. **Distributional opacity** (no aggregate relay stats published) → probabilistic ambiguity.
3. **Tunnel URL rotation** → temporal ambiguity. Breaks all correlation over time.

**Future privacy tiers (stubbed):**
- **Clean Room**: Ephemeral Docker container on provider, encrypted I/O, provider never sees plaintext. TODO: Container orchestration, key management, attestation protocol.
- **Vault / SCIF**: Wire-owned or Wire-audited hardware. Zero trust chain beyond the Wire. TODO: Hardware deployment, audit certification, TEE integration.

### How a Single Job Works (Market Path)

**The Wire coordinates and handles all credit flows. Data flows buyer → relays → seller. The Wire never touches payloads.**

1. Provider has a standing offer on the exchange: model, per-M-token-in rate, per-M-token-out rate, queue discount curve, max queue depth per model.
2. Requester submits a job (bid): model needed, max budget, relay count (from privacy policy contribution).
3. Exchange matches: bid ≥ ask (at current queue-depth-discounted rate for that model's queue).
4. **Wire charges reservation fee** from requester. Pays provider via rotator arm (76/2/2). Slot enters provider's model-specific queue.
5. Requester **fills** the slot: sends input token count (computed locally via tiktoken) + relay count to Wire. **NO prompt sent to Wire.** Wire charges token deposit + relay fees (1 per hop). Wire returns: relay chain routing instructions + provider's ephemeral encryption key.
6. **Requester sends encrypted prompt through relay chain** (data plane: requester → relay A → relay B → provider). Relays stream ciphertext without reading it. Provider decrypts, queues, GPU executes.
7. **Provider sends encrypted result back through relay chain** to requester (data plane). Requester decrypts. Chain executor task resolves.
8. **Provider reports settlement metadata to Wire** (control plane): job_id, actual prompt tokens, completion tokens, latency, finish reason. No prompt or result content ever touches the Wire.
9. **Wire settles all participants** via rotator arm:
   - Pays provider: actual token cost (76/80 slots provider, 2/80 Wire, 2/80 Graph Fund)
   - Pays each relay: per-hop fee (same 76/2/2 rotator)
   - Refunds requester: deposit overage (if actual < estimate). Wire platform absorbs underage.
   - Reservation fee: already paid at step 4 (non-refundable)
   - Relay fees: already paid at step 5 (non-refundable on completion; refundable on unfilled void)

### How Fleet-Internal Routing Works

Nodes under the same operator (same Wire account / email) bypass the exchange entirely:

1. Requester's dispatch checks the **fleet roster** first (populated from heartbeat data).
2. If a fleet node has the requested model loaded and has queue capacity → route directly to that node's tunnel URL.
3. **No Wire proxy.** Prompt goes directly from requester to fleet node. Completely private.
4. **No credits.** No reservation fee, no deposit, no settlement. It's the owner's hardware.
5. **No exchange involvement.** The Wire never sees fleet traffic. Fleet topology is invisible to the network.
6. If no fleet node is available → fall through to the market exchange (normal market path above).

**Fleet discovery — direct peer-to-peer via tunnels:**
1. Node comes online, registers with Wire. Wire responds with fleet roster (same-operator nodes + their tunnel URLs).
2. New node IMMEDIATELY announces to each fleet peer: `POST {peer_tunnel}/v1/fleet/announce { node_id, tunnel_url, models_loaded, queue_state }`
3. Fleet peers update their rosters instantly. Zero delay.
4. Future state changes (model loaded/unloaded, going offline) also announced peer-to-peer.
5. Heartbeat still carries fleet roster as a fallback (if direct announcement was missed).

**Fleet authentication:** Wire-signed fleet identity JWT. At registration/heartbeat, the Wire issues a JWT with `aud: "fleet"` containing `operator_id` and `node_id`, signed with the Wire's Ed25519 key. When Node A fleet-dispatches to Node B, it includes this JWT. Node B verifies the signature (same public key already distributed for document serving) and checks that the `operator_id` matches its own. The Wire vouches for identity (like a CA) but never sees fleet traffic. No new key management — reuses existing Ed25519 infrastructure.

### How a Single Job Works (Fleet Path)

1. Requester's dispatch policy checks fleet roster: any fleet node has model X with capacity?
2. Yes → `POST {fleet_node_tunnel}/v1/compute/job-dispatch` (same endpoint as market, but authenticated with operator credentials, no Wire job token needed).
3. Fleet node executes inference, returns result directly to requester.
4. No settlement. No credits. No Wire involvement. Cost = electricity.

### How Speculative Reservation Works

1. Requester reserves N slots on a provider. Pays N × reservation fee. Slots enter queue at positions depth+1 through depth+N.
2. Queue depth increases by N. Other buyers see deep queue, route elsewhere or get deep discount.
3. As actual calls become known (emergent from build), requester **fills** each slot. Token deposit charged per fill.
4. Unfilled slots that reach the front resolve as no-ops instantly (zero GPU time). No token deposit was charged. Reservation fee stays with provider.
5. Provider earns reservation fees for all N slots + actual token cost for filled slots. Requester pays reservation fees for all N + token cost for filled slots only.

### How the Queue Works

One FIFO queue **per loaded model** per node. Two entry points per queue:

- **Local**: Owner's builds, stale checks, queries. No credits. Enters at back of model's queue.
- **Market**: Exchange-matched jobs. Credits flow. Enters at back of model's queue.

No jumping. No preemption. Once in, position is guaranteed. GPU processes one job at a time per model queue (default). When a job completes, next job fires.

If a node has two models loaded (e.g., llama-70b and qwen-34b), it has two independent queues. Each has its own depth, its own pricing curve, its own throughput characteristics. The network observes each independently. Hardware that can run both models concurrently (e.g., two GPUs, or enough VRAM) serves both queues in parallel. Hardware that can't has two slow queues — the discount curve naturally attracts only cheap/patient work to them.

Queue depth drives pricing: the discount curve maps total depth (local + market) of THAT MODEL's queue to effective price. Deep queue = cheap. Empty queue = full price. The pricing curve IS the load balancer, per model.

### Queue Mirror Protocol

**Node → Wire (push on every queue state change):**
```
POST /api/v1/compute/queue-state
{
  node_id,
  seq,                    // monotonically increasing sequence number (Wire rejects stale pushes)
  model_queues: [         // one entry per loaded model
    {
      model_id,
      total_depth,        // local + market entries waiting (for discount curve calculation)
      market_depth,       // market entries waiting (for capacity check)
      is_executing,       // boolean: GPU currently processing a job for this model
      est_next_available_s, // seconds until next slot opens (from network-observed data)
      max_market_depth,   // policy limit per model (contribution)
      max_total_depth     // hardware limit per model (contribution)
    }
  ],
  // NOTE: no local_depth, no executing_source — prevents leaking work patterns (privacy)
  timestamp
}
```

Lightweight POST, ephemeral state. Wire holds latest snapshot per node. Old snapshots overwrite.

**Wire → Node (push job to tunnel URL — prompt content stripped of requester identity):**
```
POST {tunnel_url}/v1/compute/job-dispatch
{
  job_id,
  reservation_only,       // true if slot reserved but not yet filled
  model,                  // (present if filled)
  messages,               // (present if filled — forwarded by Wire, requester unknown to provider)
  temperature,            // (present if filled)
  max_tokens,             // (present if filled)
  response_format,        // (present if filled)
  wire_job_token,         // JWT signed by Wire, provider verifies
  credit_rate_in_per_m,   // the matched rate
  credit_rate_out_per_m,  // the matched rate
  timeout_s,
  privacy_tier             // "standard" | "strict_fanout" | future: "clean_room" | "vault"
}
```

**Node → Wire (result return — prompt content NOT included, only result + metrics):**
```
POST /api/v1/compute/settle
{
  job_id,
  wire_job_token,
  result_content,          // the LLM output
  prompt_tokens,
  completion_tokens,
  latency_ms,
  finish_reason
}
```

### Pricing Model

Providers publish two rates (both contributions, steward-optimizable):
- **Credits per million input tokens**
- **Credits per million output tokens**

Plus a **queue discount curve** (contribution): maps queue depth to price multiplier.

```yaml
# Example provider pricing (contribution, schema_type: compute_pricing)
model: llama-3.1-70b-instruct
rate_per_m_input: 500          # 500 credits per million input tokens
rate_per_m_output: 800         # 800 credits per million output tokens
reservation_fee: 2             # 2 credits per slot (non-refundable)
queue_discount_curve:          # integer basis points (10000 = 1.0x, Pillar 9)
  - depth: 0
    multiplier_bps: 10000      # 1.0x (full price, empty queue)
  - depth: 3
    multiplier_bps: 8500       # 0.85x (15% discount)
  - depth: 8
    multiplier_bps: 6500       # 0.65x (35% discount)
  - depth: 15
    multiplier_bps: 4500       # 0.45x (55% discount)
max_queue_depth: 20
```

### Network-Observed Performance

Every completed market job produces an observation record:
- Node ID, model, input tokens, output tokens
- Time from GPU start to completion
- Tokens per second (output)
- Time to first token

Wire aggregates per node per model across time horizons (hour/day/week):
- Median, p25, p75, p95 latency
- Median tokens/sec
- Segmented by input size bucket (small/medium/large)

This data flows:
- **To the market**: Queue wait estimates, provider speed rankings
- **To the node** (in heartbeat): "Here's your performance profile"
- **To deposit estimation**: Network median output for this model + input size = expected output tokens for deposit calculation

### Design Decisions

**DD-1: Graph Fund identity.** Settlement uses handle `agentwiregraphfund` — resolved to agent_id at settlement time via standard handle resolution. Not a hardcoded UUID.

**DD-2: Review sample rate.** Configurable as an `economic_parameter` contribution. No hardcoded default. Initial value set by first contribution, supersedable.

**DD-3: Bridge margin.** Pure market pricing. No platform enforcement. Providers set whatever strategy they want. The market sorts it out.

**DD-4: Sentinel model.** Configurable per node (contribution-driven). No standard model mandated.

**DD-5: Steward publication.** Operator decides publication policy (contribution). Not auto-published.

**DD-6: Deposit percentage.** The ratio of estimated cost that's locked as deposit. Initially 100% (full prepayment of estimate). Stored as `economic_parameter` contribution, supersedable as market matures. Could decrease to 50%, 20% as trust builds.

**DD-7: Negative balances.** Allowed on settlement when actual > estimate. Mildly inflationary (Wire may create credits to cover delta). Bounded: delta is small per job. Self-corrects as network estimates improve.

**DD-8: Unfilled reservation resolution.** Unfilled slots resolve instantly as no-ops when they reach queue front. No token deposit charged (none was paid). Reservation fee stays with provider. Queue depth drops, market capacity returns naturally.

**DD-9: Economic gates, not rate limits.** Every market operation costs at least 1 credit. No zero-cost operations. No traditional rate limiting. Following the query governor pattern:
- **Match attempt**: 1 credit search fee. Refunded into the reservation fee on successful match. Lost on no-match. Spamming failed matches destroys attacker's credits.
- **Queue state push**: 1 credit per push. Batched — one push per state change window, not per event.
- **Fill**: the deposit charge IS the gate.
- **Offer creation**: 1 credit. Prevents offer spam.
- All operations generate ledger entries → audit trail → statistical traceability → pattern detection at normal query cost (1 credit).
The governor curve makes sustained abuse exponentially expensive. Game theory makes attacks self-funding for the defense (Pillar 11). No artificial throttling needed.

**DD-10: Steward-mediated operation.** The operator does not manually manage market participation. The steward acts autonomously within the operator's experimental territory, then reports what it did and why. The operator is the boss (sets direction, approves territory, redirects when needed), not the manager (approves each action). For Phases 1-6 before the real steward: the generative config UI (intent → YAML → accept) serves as a proto-steward. Phase 7+ transitions to continuous autonomous management with status reporting.

**DD-11: OpenRouter dual-use.** Personal use (own builds, cloud models, bypasses market) and bridge mode (sell capacity to network for credits) are independent configurations. Both can be active simultaneously.

---

## II. Wire-Side Schema

### Prerequisites Migration

Before the compute market tables, a prerequisite migration must:

1. **Extend `wire_graph_fund.source_type` CHECK** to include `'compute_service'`, `'compute_reservation'`, `'storage_serve'`, `'hosting_grant'`, `'relay_hop'`. This is the ONE consolidated migration — storage and relay plans reference this, not their own separate CHECK extensions.

2. **Resolve operator_id path**: The offer creation endpoint resolves `wire_nodes.agent_id → wire_agents.operator_id → wire_operators.id` and stores the result on the offer row. This is a denormalization for query performance — the canonical path remains the agent→operator join.

3. **Create rotator arm infrastructure for compute market:**
```sql
-- Per-(node, model) rotator state for settlement Graph Fund levy
-- Separate rotator for reservation fee Graph Fund levy
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

-- Advance rotator and return new position
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

-- Three-way recipient determination: reads slot counts from economic_parameter contribution.
-- Default 76/2/2 but supersedable via the market_rotator_config contribution (seed #10).
-- Returns: 'provider', 'wire', or 'graph_fund'
CREATE OR REPLACE FUNCTION market_rotator_recipient(
  p_position INTEGER
) RETURNS TEXT
LANGUAGE plpgsql STABLE AS $$
DECLARE
  v_total INTEGER;
  v_wire INTEGER;
  v_gf INTEGER;
BEGIN
  -- Read slot counts from economic_parameter contribution (Pillar 37: no hardcoded numbers)
  -- Falls back to 80/2/2 ONLY if contribution doesn't exist (bootstrap).
  -- The contribution is seeded in the prerequisites migration (seed #10).
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

  -- Fallback if no contribution exists yet
  v_total := COALESCE(v_total, 80);
  v_wire := COALESCE(v_wire, 2);
  v_gf := COALESCE(v_gf, 2);

  -- Bjorklund even distribution
  -- With defaults (80/2/2): Wire at positions 40, 80. GF at positions 20, 60.
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

4. **Create Wire platform operator entity** for absorbing estimation risk:
   - Register handle `agentwireplatform` (operator-level, Wire-owned)
   - This entity's balance may go negative (estimation subsidies debit it)
   - Replenished from platform revenue (credit purchases, Graph Fund overflow)
   - Used ONLY for `compute_estimation_subsidy` debits — not for general minting

5. **`graph_fund_slot` column**: included in the `wire_compute_jobs` CREATE TABLE (not a separate ALTER — the table is new, so the column goes in the initial definition).

### New Tables

```sql
-- Provider standing offers on the exchange
-- Each row is one model offered by one node at specific rates
-- NOTE: operator_id is denormalized from wire_nodes.agent_id → wire_agents.operator_id
-- and resolved at offer creation time by the API endpoint.
CREATE TABLE wire_compute_offers (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  node_id               UUID NOT NULL REFERENCES wire_nodes(id),
  operator_id           UUID NOT NULL REFERENCES wire_operators(id),
  model_id              TEXT NOT NULL,
  provider_type         TEXT NOT NULL DEFAULT 'local',  -- 'local' | 'bridge'
  rate_per_m_input      INTEGER NOT NULL,   -- credits per million input tokens
  rate_per_m_output     INTEGER NOT NULL,   -- credits per million output tokens
  reservation_fee       INTEGER NOT NULL DEFAULT 0,
  -- Queue discount curve stored as JSONB array of {depth, multiplier}
  queue_discount_curve  JSONB NOT NULL DEFAULT '[]'::jsonb,
  max_queue_depth       INTEGER NOT NULL DEFAULT 20,
  -- Live state (updated by queue mirror, NOT contributions)
  current_queue_depth   INTEGER NOT NULL DEFAULT 0,
  status                TEXT NOT NULL DEFAULT 'active',  -- 'active'|'inactive'|'offline'
  -- Network-observed performance (aggregated by Wire)
  observed_median_tps   REAL,              -- tokens per second (output)
  observed_p95_latency_ms INTEGER,         -- p95 job completion time
  observed_job_count    INTEGER DEFAULT 0, -- total completed jobs (for confidence)
  --
  context_window        INTEGER,
  -- Privacy capabilities (stub for future tiers)
  privacy_capabilities  TEXT[] DEFAULT '{standard}',  -- TODO: add 'clean_room', 'vault' when implemented
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
  -- Bridge+local coexistence: same model can be offered as both local and bridge
  UNIQUE(node_id, model_id, provider_type)
);

ALTER TABLE wire_compute_offers ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_offers TO service_role;

CREATE INDEX idx_compute_offers_model ON wire_compute_offers(model_id)
  WHERE status = 'active';
CREATE INDEX idx_compute_offers_node ON wire_compute_offers(node_id);

-- Individual compute jobs (the atomic unit)
CREATE TABLE wire_compute_jobs (
  id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  -- Participants
  requester_node_id       UUID REFERENCES wire_nodes(id),
  requester_operator_id   UUID NOT NULL REFERENCES wire_operators(id),
  provider_node_id        UUID NOT NULL REFERENCES wire_nodes(id),
  provider_operator_id    UUID NOT NULL REFERENCES wire_operators(id),
  offer_id                UUID NOT NULL REFERENCES wire_compute_offers(id),
  -- Model and pricing at match time
  model_id                TEXT NOT NULL,
  matched_rate_in_per_m   INTEGER NOT NULL,
  matched_rate_out_per_m  INTEGER NOT NULL,
  matched_queue_depth     INTEGER NOT NULL,  -- depth when matched (determines discount)
  matched_multiplier_bps  INTEGER NOT NULL,  -- discount multiplier in basis points (10000 = 1.0x, 8500 = 0.85x)
  reservation_fee         INTEGER NOT NULL DEFAULT 0,
  -- Lifecycle
  status                  TEXT NOT NULL DEFAULT 'reserved',
  -- 'reserved' → slot in queue, maybe empty
  -- 'filled'   → prompt submitted, deposit charged
  -- 'executing' → GPU processing
  -- 'completed' → result delivered, settled
  -- 'failed'    → provider timeout/error
  -- 'cancelled' → requester cancelled before execution
  -- 'void'      → unfilled slot resolved as no-op
  -- Relay
  relay_count             INTEGER NOT NULL DEFAULT 0,  -- requester's chosen relay hops
  -- NOTE: NO prompt/payload columns. The Wire NEVER has the prompt.
  -- Prompts flow requester → relay chain → provider (data plane).
  -- The Wire only handles matching, routing, and settlement (control plane).
  -- Input token count reported by requester (computed via tiktoken locally).
  input_token_estimate    INTEGER,  -- requester-reported, used for deposit calculation
  temperature             REAL,     -- forwarded to provider in routing instructions
  max_tokens              INTEGER,  -- forwarded to provider in routing instructions
  -- Financial
  deposit_amount          INTEGER,   -- estimated token cost, locked on fill
  actual_cost             INTEGER,   -- calculated on completion
  graph_fund_levy         INTEGER,   -- 2.5% of actual_cost
  provider_payout         INTEGER,   -- actual_cost - graph_fund_levy
  requester_refund        INTEGER,   -- deposit - actual_cost (negative = additional charge)
  -- Rotator arm
  graph_fund_slot         BOOLEAN NOT NULL DEFAULT false,  -- true when this job's payout went to Graph Fund/Wire
  -- Result (result_content NOT stored — delivered transiently via relay chain, then discarded)
  result_prompt_tokens    INTEGER,
  result_completion_tokens INTEGER,
  result_latency_ms       INTEGER,
  result_finish_reason    TEXT,
  -- Timestamps
  created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
  filled_at               TIMESTAMPTZ,
  dispatched_at           TIMESTAMPTZ,
  completed_at            TIMESTAMPTZ,
  timeout_at              TIMESTAMPTZ,
  -- Batch tracking (optional, for speculative reservations)
  batch_id                UUID,       -- groups related reservations
  queue_position          INTEGER     -- position when matched
);

CREATE INDEX idx_compute_jobs_provider ON wire_compute_jobs(provider_node_id, status);
CREATE INDEX idx_compute_jobs_requester ON wire_compute_jobs(requester_operator_id, status);
CREATE INDEX idx_compute_jobs_batch ON wire_compute_jobs(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX idx_compute_jobs_timeout ON wire_compute_jobs(timeout_at) WHERE status IN ('executing', 'filled');

ALTER TABLE wire_compute_jobs ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_jobs TO service_role;

-- Performance observations (append-only, source of truth for network-measured perf)
CREATE TABLE wire_compute_observations (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  job_id          UUID NOT NULL REFERENCES wire_compute_jobs(id),
  node_id         UUID NOT NULL REFERENCES wire_nodes(id),
  model_id        TEXT NOT NULL,
  input_tokens    INTEGER NOT NULL,
  output_tokens   INTEGER NOT NULL,
  latency_ms      INTEGER NOT NULL,    -- GPU start to completion
  tokens_per_sec  REAL NOT NULL,       -- output tokens / (latency_ms / 1000)
  time_to_first_token_ms INTEGER,      -- if measurable
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_compute_obs_node_model ON wire_compute_observations(node_id, model_id);
CREATE INDEX idx_compute_obs_model ON wire_compute_observations(model_id, created_at DESC);

ALTER TABLE wire_compute_observations ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_compute_observations TO service_role;

-- Queue state mirror: per-model queue state per node.
-- One row per (node, model) — not one row per node.
-- PRIVACY: Minimized to only what the exchange needs.
CREATE TABLE wire_compute_queue_state (
  node_id              UUID NOT NULL REFERENCES wire_nodes(id),
  model_id             TEXT NOT NULL,
  seq                  BIGINT NOT NULL DEFAULT 0,  -- monotonic per (node, model); Wire rejects stale
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

-- NOTE: No wire_compute_review_batches table. Quality enforcement uses existing
-- challenge panel infrastructure (Pillar 24) + steward quality publications.
-- See Phase 5 reconception in Addendum X.
```

### Settlement RPC

**CRITICAL PATTERN**: All credit operations MUST use the existing atomic RPCs from credit-engine
(`credit_operator_atomic`, `debit_operator_atomic`). These handle `balance_after` computation,
ledger integrity, UUID validation, and category tagging. The compute market RPCs MUST NOT
write directly to `wire_operators` or `wire_credits_ledger`. Follow the pattern in
`settle_document_serve` from `20260315990000_storage_network_rpcs.sql`.

All RPCs require `SECURITY DEFINER` and corresponding `GRANT EXECUTE ... TO service_role`.

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
  -- Resolve Wire platform operator once (for estimation subsidy + Wire take)
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  -- Lock the job row
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'executing'
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in executing status';
  END IF;

  -- Guard: cap completion_tokens at 2x max_tokens to prevent absurd reports
  IF v_job.max_tokens IS NOT NULL AND p_completion_tokens > v_job.max_tokens * 2 THEN
    RAISE EXCEPTION 'Reported completion_tokens (%) exceeds plausible limit', p_completion_tokens;
  END IF;

  -- Calculate actual cost from measured tokens (ALL integer arithmetic, Pillar 9)
  -- Basis points: 10000 = 1.0x, 8500 = 0.85x. No REAL/float anywhere in financial path.
  v_actual_cost := CEIL(p_prompt_tokens::NUMERIC * v_job.matched_rate_in_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000))
                 + CEIL(p_completion_tokens::NUMERIC * v_job.matched_rate_out_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000));

  -- Platform levy: ROTATOR ARM (Pillar 9). 80-slot Bjorklund cycle:
  -- 76 provider slots (95%), 2 Wire slots (2.5%), 2 Graph Fund slots (2.5%).
  -- Each settlement advances the rotator. On a Wire/GF slot, full actual_cost goes
  -- to that recipient; provider gets zero for that job. Over 80 jobs, provider gets 95%.
  -- Pure integer economics. No rounding. No percentage math.
  -- Slot counts are economic_parameter contributions (Pillar 37).
  v_rotator_pos := advance_market_rotator(v_job.provider_node_id, 'compute', v_job.model_id, 'settlement');
  v_slot_recipient := market_rotator_recipient(v_rotator_pos);  -- reads slot counts from economic_parameter contribution
  -- Returns: 'provider' (76 slots), 'wire' (2 slots), 'graph_fund' (2 slots)

  IF v_slot_recipient = 'graph_fund' THEN
    v_graph_fund := v_actual_cost;
    v_wire_take := 0;
    v_provider_payout := 0;
  ELSIF v_slot_recipient = 'wire' THEN
    v_graph_fund := 0;
    v_wire_take := v_actual_cost;
    v_provider_payout := 0;
  ELSE  -- 'provider' (76/80 = 95%)
    v_graph_fund := 0;
    v_wire_take := 0;
    v_provider_payout := v_actual_cost;
  END IF;

  -- Deposit reconciliation: user NEVER pays more than the estimate.
  -- If actual > deposit: Wire platform absorbs the difference (debit from Wire platform operator).
  -- If actual < deposit: refund the overage to the user.
  -- Provider always gets paid full actual_cost (on non-Graph-Fund slots).
  -- The Wire platform operator (`agentwireplatform` handle) absorbs estimation risk.
  -- Its balance may go negative — replenished from platform revenue.
  -- This is bounded (delta between estimate and actual) and self-corrects
  -- as the network accumulates observations and estimates improve.
  v_requester_adj := COALESCE(v_job.deposit_amount, 0) - v_actual_cost;

  IF v_requester_adj < 0 THEN
    -- Estimate was too low. Wire platform absorbs the difference.
    -- Requester is NOT charged extra. Their cost was locked at the deposit.
    v_wire_subsidy := ABS(v_requester_adj);
    v_requester_adj := 0;  -- requester pays exactly the deposit, nothing more

    -- Debit from Wire platform operator (handle: agentwireplatform)
    -- This entity absorbs estimation risk. May go negative. Replenished from platform revenue.
    SELECT o.id INTO v_wire_platform_operator_id
      FROM wire_operators o
      JOIN wire_agents a ON a.operator_id = o.id
      JOIN wire_handles h ON h.agent_id = a.id
      WHERE h.handle = 'agentwireplatform' AND h.status = 'active'
      LIMIT 1;

    IF v_wire_platform_operator_id IS NOT NULL THEN
      -- EXPLICIT EXCEPTION to C1 audit rule ("no raw writes to wire_operators"):
      -- The Wire platform operator's balance MAY go negative (it absorbs estimation risk).
      -- debit_operator_atomic rejects negative balances by design. This is the ONE case
      -- where raw SQL is correct: the platform operator is special (it can go negative,
      -- replenished from platform revenue). A debit_operator_uncapped variant should be
      -- created during implementation to make this clean, but raw SQL is acceptable here
      -- because the platform operator identity is verified above.
      UPDATE wire_operators SET credit_balance = credit_balance - v_wire_subsidy
        WHERE id = v_wire_platform_operator_id;
      INSERT INTO wire_credits_ledger (operator_id, amount, reason, reference_id, category, balance_after)
        VALUES (v_wire_platform_operator_id, -v_wire_subsidy, 'compute_estimation_subsidy', p_job_id, 'compute_market',
                (SELECT credit_balance FROM wire_operators WHERE id = v_wire_platform_operator_id));
    END IF;
  END IF;

  -- Update job record (NO prompt data exists on Wire — Wire is pure control plane)
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
    completed_at = now()
  WHERE id = p_job_id;

  -- Pay the rotator-determined recipient
  IF v_provider_payout > 0 THEN
    PERFORM credit_operator_atomic(v_job.provider_operator_id, v_provider_payout,
      'compute_serve', p_job_id, 'compute_market');
  END IF;

  IF v_wire_take > 0 THEN
    -- Wire platform receives its 2.5% (2/80 slots)
    PERFORM credit_operator_atomic(v_wire_platform_operator_id, v_wire_take,
      'compute_wire_take', p_job_id, 'compute_market');
  END IF;

  IF v_graph_fund > 0 THEN
    INSERT INTO wire_graph_fund (amount, source_type, reference_id)
      VALUES (v_graph_fund, 'compute_service', p_job_id);
  END IF;

  -- Pay relay nodes (each hop settles independently via rotator arm)
  -- Relay fees were already charged from requester at fill time.
  -- Now pay the relays who actually forwarded.
  IF v_job.relay_count > 0 THEN
    -- The API route handler calls settle_relay_hop for each relay in the chain
    -- after confirming the job completed successfully. Not done in this RPC
    -- to keep settlement atomic per-participant. See route handler.
    NULL;  -- relay settlement is per-hop, handled by the API route
  END IF;

  -- Reconcile requester: refund deposit overage only. Never charge extra (Wire absorbs underage).
  IF v_requester_adj > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_requester_adj,
      'compute_refund', p_job_id, 'compute_market');
  END IF;
  -- No ELSIF for negative — user never pays more than estimate. Wire absorbed it above.
  -- Relay fees are NOT refunded on completion (relays did their work).
  -- Relay fees ARE refunded if the reservation expires unfilled (void path).

  -- Record observation for network performance tracking
  INSERT INTO wire_compute_observations (job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec)
    VALUES (p_job_id, v_job.provider_node_id, v_job.model_id, p_prompt_tokens, p_completion_tokens, p_latency_ms,
            CASE WHEN p_latency_ms > 0 THEN p_completion_tokens::REAL / (p_latency_ms::REAL / 1000) ELSE 0 END);

  -- Decrement queue depth
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;

  RETURN QUERY SELECT v_actual_cost, v_provider_payout, v_requester_adj;
END;
$$;

GRANT EXECUTE ON FUNCTION settle_compute_job TO service_role;
```

### Fail RPC

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

  -- Refund deposit to requester (if deposit was charged)
  IF COALESCE(v_job.deposit_amount, 0) > 0 THEN
    PERFORM credit_operator_atomic(v_job.requester_operator_id, v_job.deposit_amount,
      'compute_fail_refund', p_job_id, 'compute_market');
  END IF;

  -- Reservation fee stays with provider (non-refundable, they held capacity)

  -- Update job
  UPDATE wire_compute_jobs SET
    status = 'failed',
    result_finish_reason = p_reason,
    completed_at = now()
    -- NOTE: no messages column exists (Wire never has prompts — relay-first model)
  WHERE id = p_job_id;

  -- Decrement queue depth
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;

  -- Record failure observation (impacts provider's performance metrics)
  INSERT INTO wire_compute_observations (job_id, node_id, model_id, input_tokens, output_tokens, latency_ms, tokens_per_sec)
    VALUES (p_job_id, v_job.provider_node_id, v_job.model_id, 0, 0, 0, 0);
END;
$$;

GRANT EXECUTE ON FUNCTION fail_compute_job TO service_role;
```

### Void RPC (unfilled reservation reaches queue front)

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

  -- No deposit was charged (slot was unfilled). Reservation fee already with provider.
  UPDATE wire_compute_jobs SET status = 'void', completed_at = now()
    WHERE id = p_job_id;

  -- Decrement queue depth
  UPDATE wire_compute_queue_state SET
    market_depth = GREATEST(market_depth - 1, 0),
    total_depth = GREATEST(total_depth - 1, 0),
    updated_at = now()
  WHERE node_id = v_job.provider_node_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = GREATEST(current_queue_depth - 1, 0),
    updated_at = now()
  WHERE id = v_job.offer_id;
END;
$$;

GRANT EXECUTE ON FUNCTION void_compute_job TO service_role;
```

### Timeout Sweep (scheduled function)

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

### Offer Liveness (mark offline when heartbeat stale)

```sql
CREATE OR REPLACE FUNCTION deactivate_stale_compute_offers(
  p_stale_threshold_minutes INTEGER DEFAULT 5
) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
  v_count INTEGER;
BEGIN
  UPDATE wire_compute_offers SET status = 'offline', updated_at = now()
  WHERE status = 'active'
    AND node_id IN (
      SELECT id FROM wire_nodes
      WHERE last_seen_at < now() - (p_stale_threshold_minutes || ' minutes')::interval
    );
  GET DIAGNOSTICS v_count = ROW_COUNT;
  RETURN v_count;
END;
$$;

GRANT EXECUTE ON FUNCTION deactivate_stale_compute_offers TO service_role;
```

### Matching RPC

```sql
CREATE OR REPLACE FUNCTION match_compute_job(
  p_requester_operator_id UUID,
  p_requester_node_id UUID,      -- nullable (API callers may not be nodes)
  p_model_id TEXT,
  p_max_budget INTEGER,          -- max credits requester will pay (total including reservation)
  p_input_tokens INTEGER,        -- known (from the prompt)
  p_latency_preference TEXT DEFAULT 'best_price'  -- 'immediate' | 'best_price' | 'balanced'
) RETURNS TABLE(
  job_id UUID,
  matched_rate_in INTEGER,
  matched_rate_out INTEGER,
  matched_multiplier_bps INTEGER,
  reservation_fee INTEGER,
  estimated_deposit INTEGER,
  queue_position INTEGER
  -- NOTE: provider_tunnel_url NOT returned to requester (privacy: requester must not know provider)
  -- The Wire uses the tunnel_url internally when dispatching the prompt.
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
BEGIN
  -- Resolve Wire platform operator once
  SELECT o.id INTO v_wire_platform_operator_id FROM wire_operators o
    JOIN wire_agents a ON a.operator_id = o.id
    JOIN wire_handles h ON h.agent_id = a.id
    WHERE h.handle = 'agentwireplatform' AND h.status = 'active' LIMIT 1;

  -- Get network median output for this model (from observations)
  SELECT COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY output_tokens), 500)::INTEGER
    INTO v_est_output
    FROM wire_compute_observations
    WHERE model_id = p_model_id
      AND created_at > now() - interval '7 days';

  -- Find best matching offer based on latency preference
  -- CRITICAL: FOR UPDATE on BOTH offer and queue state rows to prevent concurrent over-assignment.
  -- Two concurrent matches for different models on the same node share the queue state row.
  -- Also: staleness check — only match against nodes with fresh queue mirror.
  -- Find best offer. SELECT o.* INTO v_offer (full row), then separately lock queue state.
  -- Can't SELECT o.*, q.* INTO two ROWTYPEs in one query — plpgsql limitation.
  SELECT o.* INTO v_offer
    FROM wire_compute_offers o
    JOIN wire_compute_queue_state q ON q.node_id = o.node_id AND q.model_id = o.model_id
    WHERE o.model_id = p_model_id
      AND o.status = 'active'
      AND q.total_depth < o.max_queue_depth
      AND q.market_depth < q.max_market_depth
      AND q.updated_at > now() - interval '2 minutes'  -- staleness check
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

  -- Lock the queue state row separately (serializes all matches to same node+model)
  SELECT * INTO v_queue FROM wire_compute_queue_state
    WHERE node_id = v_offer.node_id AND model_id = v_offer.model_id
    FOR UPDATE;

  -- Calculate new depth BEFORE the INSERT (v_new_depth must not be NULL)
  v_new_depth := v_queue.total_depth + 1;

  -- Calculate queue-depth discount multiplier (integer basis points, Pillar 9)
  v_multiplier_bps := compute_queue_multiplier_bps(v_offer.queue_discount_curve, v_queue.total_depth);

  -- Calculate estimated cost
  v_reservation := v_offer.reservation_fee;
  v_est_cost := CEIL(p_input_tokens::NUMERIC * v_offer.rate_per_m_input * v_multiplier_bps / (1000000::NUMERIC * 10000))
              + CEIL(v_est_output::NUMERIC * v_offer.rate_per_m_output * v_multiplier_bps / (1000000::NUMERIC * 10000));
  v_total_est := v_reservation + v_est_cost;

  -- Check budget
  IF v_total_est > p_max_budget THEN
    RAISE EXCEPTION 'Estimated cost (%) exceeds budget (%)', v_total_est, p_max_budget;
  END IF;

  -- Create job record FIRST so we have the job_id for ledger entries
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

  -- Charge reservation fee from requester using existing atomic RPC
  PERFORM debit_operator_atomic(p_requester_operator_id, v_reservation,
    'compute_reservation', v_job_id, 'compute_market');

  -- Pay reservation fee via rotator arm (same 76/2/2 split as settlement).
  -- On Wire/GF slots, fee goes to Wire or Graph Fund instead of provider.
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

  -- Increment queue depth (v_new_depth already computed above, before INSERT)
  UPDATE wire_compute_queue_state SET
    market_depth = market_depth + 1,
    total_depth = total_depth + 1,
    updated_at = now()
  WHERE node_id = v_offer.node_id;

  UPDATE wire_compute_offers SET
    current_queue_depth = v_new_depth,
    updated_at = now()
  WHERE id = v_offer.id;

  -- NOTE: Job INSERT moved above credit operations so v_job_id is available for ledger entries.
  -- No retroactive ledger reference_id updates needed.

  RETURN QUERY SELECT v_job_id,
    v_offer.rate_per_m_input, v_offer.rate_per_m_output,
    v_multiplier_bps, v_reservation, v_est_cost, v_new_depth;
END;
$$ LANGUAGE plpgsql;

-- Helper: interpolate queue discount curve, returns INTEGER basis points (Pillar 9)
-- 10000 = 1.0x (no discount), 8500 = 0.85x (15% discount)
-- Curve JSONB stores {depth, multiplier_bps} entries (integer basis points)
CREATE OR REPLACE FUNCTION compute_queue_multiplier_bps(
  p_curve JSONB,
  p_depth INTEGER
) RETURNS INTEGER AS $$
DECLARE
  v_prev_depth INTEGER := 0;
  v_prev_bps INTEGER := 10000;  -- 1.0x default
  v_next_depth INTEGER;
  v_next_bps INTEGER;
  v_entry JSONB;
BEGIN
  IF p_curve IS NULL OR jsonb_array_length(p_curve) = 0 THEN
    RETURN 10000;  -- 1.0x
  END IF;

  FOR v_entry IN SELECT * FROM jsonb_array_elements(p_curve) ORDER BY (value->>'depth')::INTEGER
  LOOP
    v_next_depth := (v_entry->>'depth')::INTEGER;
    v_next_bps := (v_entry->>'multiplier_bps')::INTEGER;

    IF p_depth <= v_next_depth THEN
      -- Integer linear interpolation
      IF v_next_depth = v_prev_depth THEN RETURN v_next_bps; END IF;
      RETURN v_prev_bps + (v_next_bps - v_prev_bps) *
             (p_depth - v_prev_depth) / (v_next_depth - v_prev_depth);
    END IF;

    v_prev_depth := v_next_depth;
    v_prev_bps := v_next_bps;
  END LOOP;

  -- Beyond last curve point: use last value
  RETURN v_prev_bps;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

GRANT EXECUTE ON FUNCTION compute_queue_multiplier_bps TO service_role;
```

### Fill Job RPC

**Purely financial.** The Wire NEVER receives the prompt. The requester computes input tokens
locally via tiktoken, reports the count, and the Wire charges the deposit. The Wire returns
routing instructions (relay chain + provider encryption key). The requester then sends the
encrypted prompt through the relay chain directly to the provider.

```sql
CREATE OR REPLACE FUNCTION fill_compute_job(
  p_job_id UUID,
  p_requester_operator_id UUID,
  p_input_token_count INTEGER,          -- computed by requester via tiktoken (NOT estimated)
  p_relay_count INTEGER DEFAULT 0       -- requester's chosen relay hops
) RETURNS TABLE(
  deposit_charged INTEGER,
  relay_chain JSONB,                    -- routing instructions (relay tunnel URLs, nested tokens)
  provider_ephemeral_pubkey TEXT,       -- for E2E encryption of prompt
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
BEGIN
  SELECT * INTO v_job FROM wire_compute_jobs
    WHERE id = p_job_id AND status = 'reserved'
      AND requester_operator_id = p_requester_operator_id
    FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Job not found or not in reserved status';
  END IF;

  -- Network median output for this model
  SELECT COALESCE(percentile_cont(0.5) WITHIN GROUP (ORDER BY output_tokens), 500)::INTEGER
    INTO v_est_output
    FROM wire_compute_observations
    WHERE model_id = v_job.model_id AND created_at > now() - interval '7 days';

  -- Calculate deposit from requester-reported input tokens (ALL integer arithmetic)
  v_deposit := CEIL(p_input_token_count::NUMERIC * v_job.matched_rate_in_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000))
             + CEIL(v_est_output::NUMERIC * v_job.matched_rate_out_per_m * v_job.matched_multiplier_bps / (1000000::NUMERIC * 10000));

  -- Select relay chain if relay_count > 0
  IF p_relay_count > 0 THEN
    FOR v_relay IN
      SELECT * FROM select_relay_chain(v_job.requester_node_id, v_job.provider_node_id, p_relay_count)
    LOOP
      v_relay_fee := v_relay_fee + v_relay.relay_rate;
      -- Charge relay fee from requester → Wire platform (escrow until relay settles)
      PERFORM debit_operator_atomic(p_requester_operator_id, v_relay.relay_rate,
        'compute_relay_fee', p_job_id, 'relay_market');
      PERFORM credit_operator_atomic(v_wire_platform_operator_id, v_relay.relay_rate,
        'relay_escrow_hold', p_job_id, 'relay_market');
    END LOOP;
  END IF;

  -- Charge deposit from requester
  PERFORM debit_operator_atomic(p_requester_operator_id, v_deposit,
    'compute_deposit', p_job_id, 'compute_market');

  -- E2E encryption keypair: WIRE generates the ephemeral keypair (not the provider).
  -- This prevents the requester from correlating key material to provider identity.
  -- Wire stores the private key temporarily (in-memory, per-job, expires on settlement).
  -- Wire delivers the private key to the provider via the job dispatch control channel.
  -- Requester encrypts with the public key. Provider decrypts with the private key.
  -- The Wire mediates the key exchange but can't decrypt (it discards the private key
  -- after delivering it to the provider — never persisted).
  v_provider_pubkey := encode(gen_random_bytes(32), 'hex');  -- placeholder: real implementation uses X25519

  -- Update job record (NO prompt data — Wire never has it)
  UPDATE wire_compute_jobs SET
    status = 'filled',
    input_token_estimate = p_input_token_count,
    relay_count = p_relay_count,
    deposit_amount = v_deposit,
    filled_at = now()
  WHERE id = p_job_id;

  -- Build relay chain routing instructions
  -- (The API route handler builds the nested JWT from the relay selection)
  v_relay_info := (SELECT jsonb_agg(jsonb_build_object(
    'hop', r.hop_index, 'tunnel_url', r.relay_tunnel_url, 'rate', r.relay_rate
  )) FROM select_relay_chain(v_job.requester_node_id, v_job.provider_node_id, p_relay_count) r);

  RETURN QUERY SELECT v_deposit, COALESCE(v_relay_info, '[]'::jsonb), v_provider_pubkey, v_relay_fee;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

GRANT EXECUTE ON FUNCTION fill_compute_job TO service_role;
```

**After fill, the requester:**
1. Encrypts the prompt with the provider's ephemeral public key
2. Sends the encrypted prompt through the relay chain (data plane)
3. Relays forward the ciphertext — they can't read it
4. Provider decrypts, runs inference, encrypts result
5. Result flows back through the relay chain to requester
6. Provider reports settlement metadata to Wire (control plane): token counts, latency
7. Wire settles: pays provider, pays relays, refunds deposit overage to requester

---

## III. Wire-Side API Routes

### Exchange Endpoints

```
POST /api/v1/compute/match          — Match a job (find provider, create reserved slot)
POST /api/v1/compute/fill           — Fill a reserved slot with a prompt
POST /api/v1/compute/cancel         — Cancel a reserved/filled (not executing) job
POST /api/v1/compute/settle         — Provider reports completion, triggers settlement
POST /api/v1/compute/void           — Provider reports unfilled slot reached front (no-op)
POST /api/v1/compute/fail           — Provider reports failure/timeout
GET  /api/v1/compute/market-surface — Aggregated view: providers per model, pricing tiers, depth
GET  /api/v1/compute/job/:id        — Job status and result
POST /api/v1/compute/reserve-batch  — Reserve N slots on a provider (speculative)
```

### Provider Management Endpoints

```
POST /api/v1/compute/offers         — Create/update standing offer
DELETE /api/v1/compute/offers/:id   — Withdraw an offer
GET  /api/v1/compute/offers/mine    — List my active offers
POST /api/v1/compute/queue-state    — Push queue state snapshot (from node)
GET  /api/v1/compute/performance/:nodeId/:modelId — Network-observed performance
```

### Heartbeat Extension

The existing heartbeat response gains a `compute_market` section:

```json
{
  "compute_market": {
    "performance_profile": {
      "llama-3.1-70b-instruct": {
        "median_tps": 42.5,
        "p95_latency_ms": 38000,
        "median_output_tokens": 650,
        "observation_count": 847,
        "input_size_buckets": {
          "small": { "median_latency_ms": 15000, "count": 312 },
          "medium": { "median_latency_ms": 28000, "count": 401 },
          "large": { "median_latency_ms": 52000, "count": 134 }
        }
      }
    },
    "market_summary": {
      "your_completed_jobs_24h": 47,
      "your_earned_credits_24h": 1250,
      "unfilled_demand": {
        "llama-3.1-70b-instruct": { "pending_bids": 12, "avg_budget": 8 }
      }
    },
    "fleet_nodes": [
      {
        "node_id": "uuid",
        "tunnel_url": "https://...",
        "models_loaded": ["llama-3.1-70b-instruct"],
        "model_queues": [
          { "model_id": "llama-3.1-70b-instruct", "total_depth": 0, "is_executing": false }
        ]
      }
    ]
  }
}
```

---

## IV. Node-Side Architecture

### New Module: `compute_market.rs`

Mirrors `market.rs` (storage daemon) pattern:

```rust
// State persisted to compute_market_state.json
pub struct ComputeMarketState {
    pub offers: HashMap<String, ComputeOffer>,     // model_id → offer
    pub active_jobs: HashMap<String, ComputeJob>,  // job_id → in-flight job
    pub total_jobs_completed: u64,
    pub total_credits_earned: i64,    // Pillar 9: integer economics
    pub session_jobs_completed: u64,
    pub session_credits_earned: i64,  // Pillar 9: integer economics
    pub is_serving: bool,                          // whether compute market is active
    pub last_evaluation_at: Option<String>,
}

pub struct ComputeOffer {
    pub model_id: String,
    pub provider_type: String,           // "local" | "bridge"
    pub rate_per_m_input: u64,
    pub rate_per_m_output: u64,
    pub reservation_fee: u64,
    pub queue_discount_curve: Vec<QueueDiscountPoint>,
    pub max_queue_depth: usize,
    pub wire_offer_id: Option<String>,   // ID on the Wire exchange
}

pub struct QueueDiscountPoint {
    pub depth: usize,
    pub multiplier: f64,
}

pub struct ComputeJob {
    pub job_id: String,
    pub model_id: String,
    pub status: ComputeJobStatus,        // Reserved, Filled, Executing, Completed, Failed
    pub system_prompt: Option<String>,
    pub user_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub wire_job_token: String,          // JWT for auth
    pub matched_rate_in: u64,
    pub matched_rate_out: u64,
    pub matched_multiplier: f64,
    pub queued_at: String,
    pub filled_at: Option<String>,
}
```

### New Module: `compute_queue.rs`

The unified queue replacing the blind semaphore:

```rust
/// Per-model queue. Each loaded model has its own independent queue.
pub struct ModelQueue {
    model_id: String,
    entries: VecDeque<QueueEntry>,
    executing: Option<QueueEntry>,
    max_total_depth: usize,              // hardware limit (from contribution)
    max_market_depth: usize,             // policy limit (from contribution)
}

/// Top-level compute queue manager: holds all per-model queues.
pub struct ComputeQueueManager {
    queues: HashMap<String, ModelQueue>,  // model_id → queue
    gpu_concurrency: usize,              // default 1 per model (from contribution)
    wire_mirror_tx: Option<mpsc::Sender<QueueSnapshot>>,
    result_channels: HashMap<String, oneshot::Sender<ComputeResult>>,  // job_id → webhook resolver
    fleet_roster: Arc<RwLock<Vec<FleetNode>>>,  // same-operator nodes
}

pub struct QueueEntry {
    pub id: String,                       // local build step ID or market job ID
    pub source: QueueSource,              // Local | Market
    pub model_required: String,
    pub estimated_gpu_time_s: f64,        // from network-observed data
    pub queued_at: Instant,
    pub payload: QueuePayload,            // the actual work (prompt, etc.)
}

pub enum QueueSource {
    Local { build_id: String, step_name: String },
    Market { job_id: String, wire_job_token: String },
    Reservation { batch_id: Option<String> },  // reserved but not yet filled
}

pub enum QueuePayload {
    Ready(LlmRequest),     // prompt available, ready to execute
    Reserved,               // slot held, prompt not yet submitted
}

impl ComputeQueueManager {
    /// Add a local job to the specified model's queue
    pub fn enqueue_local(&mut self, model_id: &str, entry: QueueEntry) -> Result<usize, QueueError>;

    /// Add a market job to the specified model's queue
    pub fn enqueue_market(&mut self, model_id: &str, entry: QueueEntry) -> Result<usize, QueueError>;

    /// Fill a reserved slot with actual prompt (finds it across all queues by job_id)
    pub fn fill_reservation(&mut self, job_id: &str, payload: LlmRequest) -> Result<(), QueueError>;

    /// Called when GPU finishes current job on a model. Returns next job to execute.
    pub fn advance(&mut self, model_id: &str) -> Option<QueueEntry>;

    /// Get current snapshot for Wire mirror (all model queues)
    pub fn snapshot(&self) -> QueueSnapshot;

    /// Register a oneshot channel for webhook result delivery
    pub fn register_result_channel(&mut self, job_id: &str, tx: oneshot::Sender<ComputeResult>);

    /// Resolve a result channel when webhook arrives (called from /v1/compute/result-delivery)
    pub fn resolve_result(&mut self, job_id: &str, result: ComputeResult) -> Result<(), QueueError>;

    /// Get fleet roster for same-operator routing
    pub fn fleet_roster(&self) -> &[FleetNode];

    /// Update fleet roster from heartbeat data
    pub fn update_fleet_roster(&mut self, fleet_nodes: Vec<FleetNode>);
}
```

### Server Endpoint: `/v1/compute/job-dispatch`

New warp route in `server.rs` for receiving jobs pushed by the Wire (Wire-proxied model — requester identity already stripped):

```rust
// In server.rs route composition:
let compute_dispatch = warp::post()
    .and(warp::path!("v1" / "compute" / "job-dispatch"))
    .and(warp::body::json())
    .and(with_state(state.clone()))
    .and_then(handle_compute_job_dispatch);

// Handler verifies Wire JWT, enqueues job, returns acceptance
async fn handle_compute_job_dispatch(
    body: ComputeJobDispatchRequest,
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify wire_job_token JWT signature (proves this came from the Wire)
    // 2. Check queue has capacity (market_capacity_remaining > 0)
    // 3. Add to compute queue (Reserved or Filled depending on payload)
    // 4. Push queue state to Wire mirror
    // 5. Return acceptance with estimated start time
    // NOTE: Provider does NOT know requester identity. Job arrived from Wire.
}
```

Also: `/v1/compute/fill` for filling reserved slots (Wire pushes fill when requester submits prompt), `/v1/compute/cancel` for cancellation.

### Result Delivery Endpoint (on REQUESTER node): `/v1/compute/result-delivery`

Webhook endpoint where the Wire pushes completed results:

```rust
let compute_result = warp::post()
    .and(warp::path!("v1" / "compute" / "result-delivery"))
    .and(warp::body::json())
    .and(with_state(state.clone()))
    .and_then(handle_compute_result_delivery);

// Wire pushes result here after settlement. Resolves the awaiting oneshot channel.
async fn handle_compute_result_delivery(
    body: ComputeResultDelivery,  // { job_id, result_content, prompt_tokens, completion_tokens, ... }
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify Wire signature on the delivery
    // 2. Look up the pending oneshot sender keyed by job_id
    // 3. Send the result through the channel (resolves WireComputeProvider.await_result)
    // 4. Return 200 OK
}
```

### Fleet Dispatch Endpoint: `/v1/compute/fleet-dispatch`

Same as `/v1/compute/job-dispatch` but authenticated with operator credentials (shared secret or operator JWT) instead of Wire job token. Used for fleet-internal routing where both nodes are the same operator:

```rust
let fleet_dispatch = warp::post()
    .and(warp::path!("v1" / "compute" / "fleet-dispatch"))
    .and(warp::body::json())
    .and(with_state(state.clone()))
    .and_then(handle_fleet_dispatch);

// Same-operator dispatch. No Wire involvement. No credits.
async fn handle_fleet_dispatch(
    body: FleetDispatchRequest,  // { model, messages, temperature, max_tokens, operator_token }
    state: SharedState,
) -> Result<impl warp::Reply, warp::Rejection> {
    // 1. Verify operator_token (proves same operator)
    // 2. Add to model's queue as Local source (no credits)
    // 3. Execute and return result synchronously (fleet calls are direct)
}
```

### GPU Processing Loop

New background task in `main.rs`:

```rust
// The GPU worker: pulls from queue, executes, reports
tauri::async_runtime::spawn(async move {
    loop {
        // Wait for a job to be ready at the front of the queue
        let entry = queue.wait_for_ready().await;

        match entry.payload {
            QueuePayload::Reserved => {
                // Unfilled reservation reached front. Resolve as void.
                if let QueueSource::Market { job_id, .. } = &entry.source {
                    report_void_to_wire(&api_url, &token, job_id).await;
                }
                queue.advance();
                push_queue_state(&state).await;
                continue;
            }
            QueuePayload::Ready(request) => {
                // Execute via existing unified LLM path
                let ctx = make_step_ctx_for_compute_job(&entry);
                let result = call_model_unified_with_audit_and_ctx(
                    &config, Some(&ctx), None,
                    &request.system_prompt, &request.user_prompt,
                    request.temperature, request.max_tokens,
                    request.response_format.as_ref(),
                    LlmCallOptions::default(),
                ).await;

                match (&entry.source, result) {
                    (QueueSource::Market { job_id, .. }, Ok(response)) => {
                        report_completion_to_wire(&api_url, &token, job_id, &response).await;
                    }
                    (QueueSource::Market { job_id, .. }, Err(e)) => {
                        report_failure_to_wire(&api_url, &token, job_id, &e).await;
                    }
                    (QueueSource::Local { .. }, Ok(response)) => {
                        // Deliver result to local build step via channel
                        entry.result_tx.send(Ok(response));
                    }
                    (QueueSource::Local { .. }, Err(e)) => {
                        entry.result_tx.send(Err(e));
                    }
                    _ => {}
                }

                queue.advance();
                push_queue_state(&state).await;
            }
        }
    }
});
```

### WireComputeProvider (Requester Side)

New provider type for the chain executor's dispatch policy:

```rust
pub struct WireComputeProvider {
    pub api_url: String,
    pub api_token: String,
    pub node_id: String,
}

impl WireComputeProvider {
    /// Three-phase call via Wire (Wire-proxied — requester never talks to provider):
    /// (1) match on Wire, (2) fill on Wire (Wire forwards to provider),
    /// (3) await webhook result delivery (Wire pushes result to our tunnel)
    pub async fn call(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        model: &str,
        temperature: f32,
        max_tokens: usize,
        max_budget: u64,
    ) -> Result<LlmResponse> {
        // Phase 1: Check fleet first (same operator, no credits, direct connection)
        if let Some(fleet_result) = self.try_fleet_dispatch(
            system_prompt, user_prompt, model, temperature, max_tokens
        ).await? {
            return Ok(fleet_result);
        }

        // Phase 2: Match on exchange (get a slot on a market provider)
        let match_result = self.match_job(model, max_budget, user_prompt).await?;

        // Phase 3: Fill (submit prompt TO WIRE — Wire forwards to provider,
        //          stripping our identity. We never know which provider serves us.)
        let fill_result = self.fill_job(
            &match_result.job_id,
            system_prompt, user_prompt,
            temperature, max_tokens,
        ).await?;

        // Phase 4: Await result via webhook. The Wire pushes the result to our
        // tunnel URL (POST /v1/compute/result-delivery). A oneshot channel keyed
        // by job_id resolves when the webhook arrives. No polling.
        let result = self.await_result(&match_result.job_id, match_result.timeout_s).await?;

        Ok(LlmResponse {
            content: result.content,
            usage: TokenUsage {
                prompt_tokens: result.prompt_tokens as i64,
                completion_tokens: result.completion_tokens as i64,
            },
            generation_id: Some(match_result.job_id),
            actual_cost_usd: None,
            provider_id: Some("wire-compute".into()),
        })
    }

    /// Check fleet roster for a same-operator node with this model loaded.
    /// If found, dispatch directly to their tunnel URL. No Wire, no credits.
    async fn try_fleet_dispatch(&self, ...) -> Result<Option<LlmResponse>> {
        let fleet = self.fleet_roster.read().await;
        let candidate = fleet.iter()
            .find(|n| n.models_loaded.contains(&model) && n.has_capacity());
        match candidate {
            Some(node) => {
                // Direct POST to fleet node's tunnel. No Wire proxy. No credits.
                let result = self.call_fleet_node(&node.tunnel_url, ...).await?;
                Ok(Some(result))
            }
            None => Ok(None) // No fleet capacity → fall through to market
        }
    }
}
```

Integrated into the dispatch policy as a provider option alongside `ollama-local` and `openrouter`.

### IPC Commands (Frontend Interface)

```rust
// Compute market management
#[tauri::command] async fn compute_market_enable(state, config) -> Result<()>;
#[tauri::command] async fn compute_market_disable(state) -> Result<()>;
#[tauri::command] async fn compute_market_get_state(state) -> Result<ComputeMarketState>;

// Offer management
#[tauri::command] async fn compute_offer_create(state, model_id, rates, curve) -> Result<ComputeOffer>;
#[tauri::command] async fn compute_offer_update(state, offer_id, rates, curve) -> Result<ComputeOffer>;
#[tauri::command] async fn compute_offer_remove(state, offer_id) -> Result<()>;
#[tauri::command] async fn compute_offers_list(state) -> Result<Vec<ComputeOffer>>;

// Queue visibility
#[tauri::command] async fn compute_queue_snapshot(state) -> Result<QueueSnapshot>;

// Market surface
#[tauri::command] async fn compute_market_surface(state, model_id) -> Result<MarketSurface>;

// Bridge config
#[tauri::command] async fn compute_bridge_enable(state, openrouter_key, models) -> Result<()>;
#[tauri::command] async fn compute_bridge_disable(state) -> Result<()>;
```

### Frontend Components — Steward-Mediated Interface

The operator interacts with the steward, not with market knobs directly. The UI is a **steward status report** with redirect capabilities. The steward acts autonomously within the operator's experimental territory, then reports what it did and why.

```
src/components/market/
  MarketDashboard.tsx           — Unified steward status report across all three markets
                                  Shows: what the steward did, what changed, earnings summary,
                                  health status, recent experiments and their outcomes.
                                  Actions: [Undo this change] [Write note to steward] [Looks good]
                                  Each action wakes the steward with the operator's instruction.

  StewardStatusReport.tsx       — "Since your last check: earned X credits, processed Y jobs,
                                  adjusted compute pricing (demand shifted 15%), loaded qwen-2.5-34b
                                  (incentive pool made it profitable), increased relay capacity
                                  (utilization was 90%). All changes within your approved territory."
                                  Expandable sections with reasoning for each decision.

  StewardDirectionPanel.tsx     — Natural language input: "Focus on maximizing compute earnings"
                                  or "Keep it conservative" or "I don't want to relay anymore."
                                  The steward reads these as boss instructions and adjusts behavior.
                                  Also: suggested one-button actions from the steward:
                                  "I recommend enabling bridge mode — your OpenRouter key could
                                  earn 40 credits/hour. [Enable] [Not now] [Tell me more]"

  QueueLiveView.tsx             — Real-time queue visualization (what's executing, what's waiting)
                                  Informational, not actionable — the steward manages the queue.

  MarketSurface.tsx             — Browse available models, providers, pricing across the network.
                                  For curiosity and context, not for manual trading.

  PerformanceProfile.tsx        — Network-observed performance: your speed, reliability,
                                  position relative to competitors. Interesting stats.

  SetupWizard.tsx               — Steward-mediated onboarding (Phase 1):
                                  Steward detects hardware → recommends SOTA config →
                                  operator confirms or redirects → steward applies.
                                  "I've detected your M2 Max with 64GB. I recommend loading
                                  qwen-2.5-32b and enabling compute + storage + relay.
                                  [Sounds good] [I'd prefer...] [Skip for now]"
```

**NOTE:** The operator does NOT manually set pricing, edit queue discount curves, or manage offers. The steward does this autonomously based on the operator's stated preferences and the experimental territory. The generative config UI (intent → YAML → accept) serves as the proto-steward for Phases 1-6: operator states intent, LLM generates config, operator accepts. The real steward (Phase 7+) takes over and manages continuously.

### OpenRouter — Dual Use

OpenRouter serves two independent purposes:
1. **Personal use**: The operator's own pyramid builds use OpenRouter models (cloud inference). This is how the app works today. Bypasses the market entirely.
2. **Bridge mode**: The operator SELLS their OpenRouter access to the network for credits. Other nodes' compute jobs get fulfilled via the operator's OpenRouter key.

These are configured independently. An operator can have personal OpenRouter AND bridge mode simultaneously — using cloud models for their own builds while selling excess capacity to the network.

---

## V. Phase Breakdown

### Phase 1: Queue & Foundation

**What ships:** The unified compute queue replaces the blind semaphore. Local builds use it. Wire mirror infrastructure ready but not connected. Compute market UI shell.

**Wire workstream:**
- Migration: `wire_compute_offers`, `wire_compute_jobs`, `wire_compute_observations`, `wire_compute_queue_state` tables
- RPCs: `compute_queue_multiplier`, basic observation aggregation queries

**Node workstream:**
- `compute_queue.rs`: `ComputeQueueManager` with per-model `ModelQueue`, FIFO ordering, `enqueue_local`/`advance`/`snapshot`
- `compute_market.rs`: `ComputeMarketState` struct, load/save to disk
- **CRITICAL MIGRATION — semaphore removal:** `LOCAL_PROVIDER_SEMAPHORE` is removed (or set to `Semaphore::new(usize::MAX)`). The queue IS the serializer. `ProviderPools` Ollama pool becomes a no-op (max permits). All three existing LLM call sites (`call_model_unified_and_ctx`, `call_model_via_registry`, direct path) stop acquiring semaphores. The GPU processing loop in the queue is the sole concurrency gate. This is a flag-day change — semaphore and queue cannot coexist (deadlock: GPU loop holds queue slot, tries to acquire semaphore held by stale engine task that's trying to enqueue).
- **Local builds bypass `max_total_depth`:** `enqueue_local` blocks (async wait) until the GPU loop has capacity, never returns QueueError. Local work is never rate-limited by the market. Only market jobs are subject to depth limits. This matches how the semaphore works today — it blocks, it doesn't reject.
- **Stale engine:** Must submit LLM calls through `enqueue_local`, not directly through the LLM path. Changes timing (stale checks now queue behind build steps) but the stale deferral system (from Ollama daemon control plane) already handles this.
- GPU processing loop in `main.rs` (pulls from queue, executes via existing LLM path — but the LLM path no longer acquires any semaphore)
- **Ollama control plane coordination:** For Phases 1-5, the compute market uses whatever model(s) `local_mode.rs` has loaded. No model switching by the market. Phase 6 introduces a `ModelManager` abstraction that sits above both.

**Frontend workstream:**
- `ComputeQueueView.tsx`: Live queue display showing what's executing and what's waiting
- Shell `ComputeMarketPanel.tsx` with enable/disable toggle

**Verification:** Local builds work exactly as before but through the queue. Queue view shows build steps processing serially.

**Seam to Phase 2:** The queue exists and works for local. Phase 2 adds the market entry point and Wire mirror.

### Phase 2: Exchange & Matching

**What ships:** Providers can publish offers. Requesters can match jobs. Wire mirror active. Jobs flow through the exchange.

**Wire workstream:**
- `POST /api/v1/compute/offers`: Create/update offers
- `POST /api/v1/compute/match`: `match_compute_job` RPC
- `POST /api/v1/compute/fill`: `fill_compute_job` RPC
- `POST /api/v1/compute/queue-state`: Receive queue snapshots
- `GET /api/v1/compute/market-surface`: Aggregated market view
- Heartbeat extension: `compute_market` section in response

**Node workstream:**
- Wire mirror: push queue state on every change via `wire_mirror_tx`
- Queue mirror loop in `main.rs`: batches and sends state to Wire
- Offer management: `compute_offer_create`/`update`/`remove` IPC commands
- `server.rs`: `/v1/compute/job-matched` endpoint (receive matched jobs from Wire)
- `compute_queue.rs`: `enqueue_market`, `fill_reservation`, market capacity checks

**Frontend workstream:**
- `ComputeOfferManager.tsx`: Create/edit offers with per-M-token pricing
- `ComputeMarketSurface.tsx`: Browse models, providers, pricing tiers
- `ComputeQueueView.tsx`: Now shows both local and market entries

**Verification:** Two nodes: Node A publishes offer, Node B matches a job. Job appears in A's queue, executes on A's GPU, result delivered to Wire.

### Phase 3: Settlement & Requester Integration

**What ships:** Full credit loop. Requester's chain executor can dispatch to Wire compute. Settlement works.

**Wire workstream:**
- `POST /api/v1/compute/settle`: `settle_compute_job` RPC
- `POST /api/v1/compute/void`: Unfilled slot resolution
- `POST /api/v1/compute/fail`: Failure handling + refund
- Performance observation recording in settlement
- Observation aggregation (median, p95, bucketed) — run on schedule or in settlement

**Node workstream:**
- `WireComputeProvider`: New provider type with fleet-first routing + three-phase market call (match + fill + await webhook)
- **Dispatch integration (separate agent):** Add `wire-compute` as recognized provider in `chain_dispatch.rs`. When dispatch policy resolves to wire-compute, delegate to `WireComputeProvider.call()` instead of the HTTP provider path. This requires a new branch in the dispatch logic, not shoehorning into `LlmProvider` trait. Add result delivery webhook endpoint (`/v1/compute/result-delivery`) with oneshot channel resolution.
- GPU processing loop: report completion/failure/void to Wire after processing
- `StepContext` for compute-served jobs (Law 4): slug="compute-market", build_id=job_id, step_name="compute-serve", depth=0. Cache key includes model+prompt hash.
- **Privacy: fill RPC return type audit.** Verify `fill_compute_job` does NOT return `provider_tunnel_url` to callers. The Wire uses the tunnel URL internally (for relay chain setup) but NEVER returns it in the API response. The RPC returns only: `(deposit_charged, relay_chain, provider_ephemeral_pubkey, total_relay_fee)`.

**Frontend workstream:**
- `ComputeEarningsTracker.tsx`: Credits earned, jobs completed, refund/overage tracking
- Build settings: Wire Compute as provider option
- Build preview: cost estimate showing compute market pricing

**Verification:** Node A does a pyramid build using Node B's GPU via the compute market. Credits flow correctly. Cache works for remote inference. Build completes and produces valid pyramid.

### Phase 4: Bridge Operations

**What ships:** Nodes can bridge cloud models to the network for credits.

**Wire workstream:**
- Bridge-specific offer fields (provider_type='bridge', source tracking)
- Dollar cost tracking (optional, for bridge operators to see their margin)

**Node workstream:**
- Bridge mode in `compute_market.rs`: receive market job → dispatch to OpenRouter → return result
- Bridge capability detection: query OpenRouter for available models, auto-generate offers
- Bridge cost tracking: compare credit revenue to dollar cost per job
- Config: bridge-specific settings (OpenRouter key, model allowlist, margin target)

**Frontend workstream:**
- `BridgeConfigPanel.tsx`: Enable bridge, configure OpenRouter key, select models to offer
- Bridge economics view: credit revenue vs dollar cost, effective margin

**Verification:** Node A runs a build using Node B as bridge. B calls OpenRouter, returns result. Credits flow to B. B can see dollar cost vs credit revenue.

### Phase 5: Quality & Challenges

**What ships:** Quality enforcement through existing primitives. No dedicated review system. Challenges handle disputes. Steward publications produce quality data naturally.

**Wire workstream:**
- Wire compute challenges to existing challenge panel infrastructure (Pillar 24)
- Timing-based anomaly detection: flag physically implausible response times (response faster than model could produce on claimed hardware)
- Reputation signals from challenge outcomes + network-observed performance (flag rate, speed percentiles)
- Clawback on malicious detection (Pillar 11) — forfeit earned credits from flagged job to fund challenger bounty

**Node workstream:**
- Challenge submission: requester's steward can challenge suspect results via existing challenge API
- Steward comparison testing: composable function that replicates inputs across providers and publishes structured quality analysis (without private context) as contributions
- Quality contribution aggregation: steward reads network-published quality data to inform provider selection

**Frontend workstream:**
- Challenge activity panel: challenges filed, outcomes, bounties earned
- Provider reputation display on market surface (from challenge outcomes + performance data)
- Steward quality publication viewer

**Verification:** Requester challenges a suspect result. Challenge panel resolves. If malicious, provider's earned credits clawback to fund challenger bounty. Steward publishes comparison data as a contribution.

**NOTE:** No `wire_compute_review_batches` table. No Wire-dispatched blind reviews. No dedicated reviewer role. Quality enforcement uses existing challenge primitives (Pillar 24) + natural steward publication behavior. The review provider class from the architecture vision doc is subsumed by stewards doing what stewards do.

### Phase 6: Daemon Intelligence

**What ships:** The daemon makes smart decisions about model portfolio, pricing, and capacity.

**Node workstream:**
- Model portfolio management: watch demand signals from heartbeat, decide which models to load
- Dynamic pricing: adjust rates based on queue utilization and market conditions
- Demand signal consumption: unfilled bids, model popularity, pricing trends
- Revenue optimization: balance storage market, compute market, and local work
- All decisions as contribution supersession chains (existing config contribution pattern)

**Frontend workstream:**
- Market analysis dashboard: demand signals, pricing trends, revenue breakdown
- Model portfolio view: loaded models, demand for each, revenue per model
- Pricing optimizer: show current curve, suggest adjustments, preview impact

**Verification:** Node adjusts pricing based on queue utilization. Loads a model because demand signals show unfilled bids. Revenue improves.

### Phase 7: Sentinel

**What ships:** 2b model runs periodic health checks on the daemon, auto-adjusts routine issues, escalates complex ones.

**Node workstream:**
- Sentinel process: configurable frequency (contribution-driven), runs local 2b model
- Health checks: queue utilization, flag rate trends, throughput drift, pricing competitiveness
- Auto-adjustment: routine fixes (bump pricing, reload a model, adjust max_market_depth)
- Escalation: signals to smart steward when judgment is needed
- Sentinel check chain: the check process is a chain YAML (contributable, improvable)

**Frontend workstream:**
- Sentinel activity log: what it checked, what it adjusted, what it escalated
- Sentinel configuration: check frequency, adjustment bounds

**Verification:** Sentinel detects throughput degradation, automatically reduces max_market_depth. Detects underpricing, escalates to steward.

### Phase 8: Smart Steward

**What ships:** Full experiment loop. Configuration as contribution supersession. Network bootstrapping.

**Node workstream:**
- Experiment loop: observe → hypothesize → change → measure → keep/revert
- Measurement windows: configurable per experiment
- Contribution publishing: share successful optimizations to Wire
- Network bootstrapping: query Wire for best configs matching hardware profile, apply as baseline
- Steward as action chains: the experimental methodology is chain YAML

**Frontend workstream:**
- Experiment log: hypotheses, changes, measurements, outcomes
- Configuration diff view: current vs baseline vs best-ever
- Network SOTA view: what configs other similar nodes are using

**Verification:** Steward runs an experiment (change pricing curve), measures revenue change, keeps improvement. Contributes the new curve. Another node adopts it.

### Phase 9: Steward Chains & Meta-Optimization

**What ships:** Steward methodology as contributable chains. Publication layer. Process-level network learning.

**Node workstream:**
- Steward behavior decomposed into separate chains: market observation, experiment design, measurement, publishing, meta-coordination
- Each chain is a contribution (forkable, improvable by the network)
- Publication chain: steward blogs with experimental analysis
- Meta-coordination: orchestrates which chains to run and when

**Wire workstream:**
- Process contribution type for steward methodology
- Publication contribution type for steward blogs
- Subscription mechanism for following steward publications

**Frontend workstream:**
- Steward publication browser
- Methodology adoption UI: browse and adopt steward chains from the network

**Verification:** Node A's steward discovers better methodology chain. Contributes it. Node B adopts it and measures improvement.

---

## VI. Workstream Structure Per Phase

Each phase uses the standard build pattern:

1. **Implementer agents** — one per focused workstream (Pillar 40). Workstreams are:
   - Wire (migrations + routes + RPCs)
   - Node backend (Rust modules)
   - Frontend (React components + IPC)

2. **Serial verifier+fixer** — receives exact same instructions and punch list as implementer. Arrives expecting to build, audits with fresh eyes, fixes in place (Pillar 39).

3. **Wanderer** — receives only the feature name and "does this actually work?" Traces end-to-end execution, catches validators/wiring/dead paths.

Workstreams within a phase run in parallel where independent. Wire migrations must land before node integration. Frontend can develop against IPC contracts in parallel with node backend.

---

## VII. Contribution Types Introduced

All new contribution types follow Law 3 (one contribution store):

| Schema Type | Purpose | Where |
|---|---|---|
| `compute_pricing` | Per-model pricing (rates, curve, reservation fee, competitive strategy) | Node config store |
| `compute_capacity` | Max market depth, max total depth, concurrency | Node config store |
| `compute_bridge` | Bridge configuration (models, margin targets) | Node config store |
| `sentinel_check` | Sentinel check chain YAML | Node config store |
| `steward_methodology` | Steward experiment chain YAML | Node config store |
| `incentive_pool` | Universal incentive pool (any criteria type, anyone can fund). See `wire_incentive_pools` table below. | Wire contributions |
| `incentive_criteria` | Defines a criteria type for incentive pools (model_availability, etc.) | Wire contributions |
| `economic_parameter` | Deposit percentage, relay threshold, rotator slot counts | Wire contributions |

---

## VIII. Pillar Conformance Notes

| Pillar | How Respected |
|---|---|
| 1 (Everything is a contribution) | Pricing, capacity, bridge config, sentinel chains, steward methodology — all contributions. Jobs are records, not contributions (operational, not intelligence). |
| 2 (All the way down) | Pricing curves, discount functions, review sample rates, deposit percentages — all contributable and supersedable. |
| 3 (Strict derived_from) | Compute jobs are service payments, not contributions with derivation chains. No derived_from applies. |
| 5 (Immutability + supersession) | Pricing changes supersede. Config changes supersede. No mutation of existing contributions. |
| 7 (UFF) | No creator/source-chain split (no 60/35 — there's no derivation chain for service payments). Wire 2.5% + Graph Fund 2.5% both apply via rotator arm (76/2/2 out of 80 slots). Provider receives 95%. |
| 9 (Integer economics) | All credit amounts are integers. Rates are per-million-tokens (large denominator avoids fractional credits). CEIL on cost calculations. Graph Fund levy uses rotator arm (2/80 slots, Bjorklund distribution). Queue discount multipliers stored as integer basis points (8500 = 0.85x) — no REAL/float in financial paths. Rust structs use i64 not f64 for credits. Rotator arm state table + functions in prerequisites migration. |
| 12 (Emergent pricing) | The order book IS emergent pricing. No Wire-set prices. |
| 14 (Handle-paths) | Graph Fund recipient addressed by handle `agentwiregraphfund`. |
| 18 (One IR, one executor) | Compute-served jobs run through `call_model_unified_with_audit_and_ctx` — same path as local builds. |
| 21 (Dual-gate) | Providers must meet economic AND reputation thresholds to publish offers. Enforced at offer creation endpoint (POST /api/v1/compute/offers). |
| 23 (Preview-then-commit) | Market surface shows pricing, queue depth, ETA before requester commits. |
| 24 (Challenge panels) | Review disputes use existing challenge infrastructure. |
| 25 (Platform agents use public API) | Wire's own compute needs go through the same exchange. |
| 35 (Graph Fund) | 2.5% via rotator arm (2/80 slots) on both settlement and reservation fees. Wire also takes 2.5% (2/80 slots). Total platform levy: 5% (4/80 slots). Provider receives 95% (76/80 slots). |
| 37 (Never prescribe outputs) | All thresholds, rates, limits are contribution-driven. No hardcoded numbers constraining behavior. |
| 38 (Fix all bugs) | — |
| 39 (Serial verifier) | Every phase gets verifier + wanderer. |
| 40 (One agent per task) | Separate agents for Wire, Node, Frontend per phase. |
| 42 (Always include frontend) | Every phase has a frontend workstream. |
| Law 1 (One executor) | All inference (local and market-served) goes through the chain executor's LLM dispatch path. |
| Law 3 (One contribution store) | All config is contributions. No new tables for user-facing data. |
| Law 4 (Every LLM call gets StepContext) | Compute-served jobs get StepContext: slug="compute-market", build_id=job_id, step_name="compute-serve", depth=0. Cache key includes model+prompt hash. Event bus emits compute-specific events. |
| Law 5 (Never prescribe outputs) | Same as Pillar 37. |

---

## IX. Stage 1 Audit Corrections Applied

**Audit date:** 2026-04-13. Two independent informed auditors. Findings merged and corrected inline.

### Critical Fixes Applied

| Finding | Fix |
|---|---|
| C1: RPCs bypass credit-engine | All RPCs rewritten to call `credit_operator_atomic`/`debit_operator_atomic`. No raw writes to `wire_operators` or `wire_credits_ledger`. |
| C2: Graph Fund CHECK constraint | Prerequisites migration section added. Must extend `wire_graph_fund.source_type` CHECK before compute tables. |
| C3: Match RPC race condition | `FOR UPDATE OF o, q` — locks both offer and queue state rows. Serializes all matches to the same node. |
| C4: Deposit % hardcoded | Noted as Phase 3 refinement: `fill_compute_job` must read `economic_parameter` contribution for deposit percentage. |
| C5: Pillar 9 Graph Fund levy | Rotator arm (Pillar 9). 2 slots out of 80. On Graph Fund slot, full payment goes to Graph Fund. On provider slot (78/80), full payment goes to provider. Pure integer. No percentage math. Rotator arm functions + state table defined in prerequisites migration. |
| C6: `operator_id` FK path | Prerequisites section documents resolution path: `wire_nodes.agent_id → wire_agents.operator_id`. Offer creation endpoint resolves and stores denormalized. |

### Major Fixes Applied

| Finding | Fix |
|---|---|
| J1: Invalid SQL ledger UPDATE | Ledger INSERTs moved after job INSERT. `v_job_id` available. No retroactive reference_id updates. |
| J2: Prompt/result persisted | No `messages` or `result_content` columns exist — Wire never has prompts (relay-first model). Result delivered transiently via relay chain, not stored. |
| J3: No RLS/GRANT/SECURITY DEFINER | Added `ENABLE ROW LEVEL SECURITY`, `GRANT ALL TO service_role` on all tables. All RPCs marked `SECURITY DEFINER` with `GRANT EXECUTE`. |
| J4: WireComputeProvider ≠ LlmProvider | Deferred to implementation — requires new dispatch path in `chain_dispatch.rs` that checks provider type and delegates to `WireComputeProvider.call()`. The three-phase call doesn't fit the standard HTTP provider trait. |
| J5: No fail/void/cancel RPCs | Added `fail_compute_job`, `void_compute_job` RPCs with full credit reconciliation and queue depth decrement. Cancel RPC spec deferred to Phase 2 implementation. |
| J6: Stale mirror → bad matches | Added `q.updated_at > now() - interval '2 minutes'` staleness check to matching RPC. |
| J7: Queue mirror leaks patterns | Minimized mirror: removed `local_depth`, `executing_source`, `executing_model`. Added `seq` for ordering, `is_executing` boolean, `est_next_available_s`. |
| J8: Negative balance uncapped | Settlement caps underage at 3x deposit (`v_max_underage`). Guard on completion_tokens (max 2x `max_tokens`). |
| J9: Reservation fee skips Graph Fund | Added Graph Fund levy on reservation fee in `match_compute_job`. |
| J10: No timeout sweep | Added `sweep_timed_out_compute_jobs()` function with `SKIP LOCKED` for safe concurrent execution. |
| J11: Stale offers from crashed nodes | Added `deactivate_stale_compute_offers()` function. Called from heartbeat handler or scheduled. |
| J12: UNIQUE blocks bridge+local | Changed to `UNIQUE(node_id, model_id, provider_type)`. |
| J13: Model thrashing | Deferred to Phase 6 (daemon intelligence). Model-aware queue scheduling noted as implementation requirement. |
| J14: `f64` credits in Rust | Changed to `i64` in `ComputeMarketState`. |
| J15: No dual-gate in matching | Documented: enforced at offer creation endpoint, not in matching RPC. |
| J16: `system_prompt`/`user_prompt` → `messages` | Schema changed to `messages JSONB`. Fill RPC builds messages array. Supports multi-turn and tool calls. |

### Known Issues Accepted (not bugs — design choices)

1. **Provider sees prompts** (standard tier). Mitigated by Wire-proxied anonymization. Clean Room/Vault stubbed for future.
2. **Reservation griefing** (flooding queues with unfilled reservations). Mitigated by reservation fee cost. Daemon intelligence (Phase 6) can detect and respond.
3. **Timing correlation** (batch patterns reveal build structure). Mitigated by `max_jobs_per_provider` fan-out policy. Enforced requester-side in `WireComputeProvider`.
4. **Compute contracts** (long-term committed capacity from vision doc). Intentionally deferred — not in any phase. Will be separate build plan.
5. **Experimental territory** (Phase 8). Noted — the Ollama control plane already built this UI. Phase 8 should wire the steward to read it.
6. **Requester restart loses in-flight results.** Oneshot channels are ephemeral. If requester node restarts mid-build, awaiting results are lost. Mitigation: the Wire should retry result delivery on failure (3 attempts with backoff). If all retries fail, the result is lost (the job completed and provider was paid, but requester never got the result). The build step times out on the requester side and the chain executor's error strategy handles it (retry → re-match → new job). Cost: requester pays twice for that one call. Acceptable for launch; persistent result queue is a Phase 3 refinement.
7. **Fleet jobs produce zero network observations.** Fleet routing bypasses the Wire entirely, so the network's performance model for that hardware is built on market jobs only. Mitigation: the node CAN optionally push fleet observations to the Wire for its own performance profile (not required, just informational). This is a Phase 4+ enhancement.
8. **Timeout sweep scheduling.** The `sweep_timed_out_compute_jobs()` function exists but must be called. Options: pg_cron (cleanest), heartbeat-probabilistic (10% chance per heartbeat, like stale work recovery), or application-level timer. Decision deferred to implementation — any of these work.
9. **Fleet dispatch failure fallback.** If fleet dispatch fails (tunnel down, timeout), immediately fall through to market. No retry on fleet — the market is the fallback. The fleet roster's 60s freshness means occasionally dispatching to a node that just went offline. The HTTP timeout (5s for fleet, since it's LAN/local) catches this quickly.
10. **Hardcoded 500-token fallback** for cold-start output estimation. Pillar 37 violation but bounded: only affects the first N jobs on a brand-new model. Should be replaced by an `economic_parameter` contribution for `default_output_estimate_tokens` per model family. Deferred to Phase 2 implementation.
11. **Provider Graph Fund slot visibility.** When a provider's payout is 0 on a Graph Fund rotator slot, the `graph_fund_slot = true` flag on the job record explains why. The frontend must surface this. Phase 2 frontend workstream.

### Stage 2 Discovery Audit Corrections Applied

**Audit date:** 2026-04-13. Two independent discovery auditors.

| Finding | Severity | Fix |
|---|---|---|
| S1: `v_new_depth` used before assignment | Critical | Moved assignment before INSERT. v_new_depth computed immediately after FOR UPDATE. |
| S2: Column name `executing_est_completion_s` doesn't exist | Critical | Changed to `est_next_available_s` (actual column name). |
| S3: `fill_compute_job` returns tunnel URL | Critical | Deferred to implementation: fill RPC must NOT return tunnel URL. Wire uses it internally only. |
| S4: `debit_operator_atomic` rejects negatives | Critical | Eliminated entirely: user never pays more than estimate. Wire absorbs underage via credit creation. No debit_operator_atomic on settlement underage path. |
| S5: Semaphore/queue deadlock | Critical | Phase 1 now explicitly removes LOCAL_PROVIDER_SEMAPHORE. Queue is sole serializer. Flag-day migration documented. |
| S6: Local builds backpressure (QueueError) | Critical | `enqueue_local` blocks (async wait), never rejects. Local bypasses max_total_depth. Only market jobs have depth limits. |
| S7: `result_content` still in schema | Major | Column removed from CREATE TABLE. Result delivered transiently via webhook. |
| S8: Graph Fund reservation levy unfunded | Major | Fixed: rotator arm for reservation fees too. On Graph Fund slot, fee goes to Graph Fund instead of provider. Fully funded. |
| S9: SELECT INTO assigns one column | Major | Split into two queries: SELECT o.* INTO v_offer, then SELECT * INTO v_queue separately. |
| S10: `matched_multiplier` as REAL/f64 | Major | Noted for implementation: should use integer basis points (8500 = 0.85x). |
| S11: Cancel RPC missing | Major | Noted: must be added in Phase 2 implementation. |
| S12: `fill_compute_job` takes strings not JSONB | Major | Noted: signature must accept `p_messages JSONB` directly. |
| S13: Heartbeat doesn't refresh queue state | Major | Noted: heartbeat handler should update `wire_compute_queue_state.updated_at` as fallback liveness. |
| S14: Ollama control plane conflict | Major | Phase 1 note: compute market uses local_mode's loaded models for Phases 1-5. ModelManager in Phase 6. |
| S15: Graph Fund levy rate hardcoded | Major | Both settlement and reservation use rotator arm (contribution-configured slot counts). Levy rate is structural (2/80), not a percentage literal. |
| S16: Phase 3 dispatch path | Major | Added explicit dispatch integration sub-workstream to Phase 3 with separate agent. |
| S17: Work engine vs compute boundary | Minor | Documented: mechanical work uses work.rs, GPU work uses compute queue. Phase 5 reviews route through compute queue. |

### Post-Stage-1 Design Refinements (from owner feedback)

| Change | Rationale |
|---|---|
| Per-model queues (not per-node) | Each loaded model has independent queue, depth, pricing. No model thrashing. Hardware with multiple models has multiple independent queues. |
| Webhook result delivery (not polling) | Every node has a tunnel. Wire pushes results to requester via `POST {tunnel}/v1/compute/result-delivery`. Zero polling. |
| Fleet-first routing | Same-operator nodes route directly, bypassing Wire entirely. No credits, no proxy. Private. Critical for multi-machine setups. |
| Rotator arm for ALL Graph Fund levies | 2/80 slots = 2.5%. Both settlement and reservation fees use rotator arm. Pure integer. No percentage math anywhere. Rotator arm infrastructure (table + functions) defined in prerequisites migration. |
| Zero hardcoded numbers | Every exchange parameter reads from a contribution: levy rate, staleness threshold, deposit %, matching weights. |

---

## X. Addendum: Post-Storage-Market Design Insights

**Date:** 2026-04-13. Insights from designing the storage market conversion that feed back into the compute market plan.

### A. Competitive Auto-Pricing (applies to compute offers)

Providers set a STRATEGY instead of a fixed rate. The daemon resolves the effective rate from market data in the heartbeat. Same primitive used in storage.

Add to `compute_pricing` contribution schema:

```yaml
# Compute pricing with competitive strategy
model: llama-3.1-70b-instruct
pricing_mode: competitive           # "fixed" | "competitive"
competitive_target: match_best      # "match_best" | "undercut_best" | "premium_over_best"
competitive_offset_bps: 0           # basis points relative to target
floor_per_m_input: 200              # never below (covers electricity)
floor_per_m_output: 300
ceiling_per_m_input: 2000           # never above (even as last provider standing)
ceiling_per_m_output: 3000
rate_per_m_input: 500               # fixed rate (used when pricing_mode = "fixed")
rate_per_m_output: 800
reservation_fee: 2
queue_discount_curve:
  - depth: 0
    multiplier_bps: 10000
  - depth: 3
    multiplier_bps: 8500
  - depth: 8
    multiplier_bps: 6500
  - depth: 15
    multiplier_bps: 4500
max_queue_depth: 20
```

The daemon resolves: `effective_rate = clamp(apply_strategy(best_market_rate, strategy), floor, ceiling)`. Pushes the resolved rate to the Wire offer. The Wire only sees the number, not the strategy. Matching stays simple.

**What this produces:** Natural price convergence. Budget providers undercut. Standard providers match. Premium providers charge above market for shorter queues. The equilibrium price is the marginal cost of the least efficient competitive provider. Price discovery without anyone setting a price.

### B. Universal Incentive Pools

Replaces the earlier concept of "compute grants" with a general primitive anyone can use:

```yaml
# Incentive pool contribution (schema_type: incentive_pool)
pool_name: "Keep llama-70b available on 5+ nodes"
criteria_type: model_availability      # contribution-defined criterion
criteria_params:
  model_id: "llama-3.1-70b-instruct"
  min_providers: 5
amount_remaining: 10000
payout_interval_s: 3600               # 1 credit/hour to each qualifying provider
status: active
```

**Key properties:**
- **Anyone can create a pool.** Not platform-specific. An operator who wants a model available funds it.
- **Anyone can stack into a pool.** Multiple funders add credits to the same pool. Total signal = sum of all funders.
- **Criteria types are contributions.** Someone invents `model_availability`, someone else invents `corpus_hosting`, someone else invents `relay_capacity`. The incentive vocabulary grows through the contribution pattern.
- **The platform uses the same mechanism.** Wire Fund Grants for cultural content are just platform-created incentive pools with `criteria_type: document_hosting`. No special infrastructure.
- **Payouts via rotator arm.** Each payout tick: 76/80 to qualifying provider, 2/80 Wire, 2/80 Graph Fund.

**Wire-side table schema:**

```sql
CREATE TABLE wire_incentive_pools (
  id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  funder_operator_id    UUID NOT NULL REFERENCES wire_operators(id),
  criteria_type         TEXT NOT NULL,          -- references an incentive_criteria contribution
  criteria_params       JSONB NOT NULL,         -- type-specific params (model_id, corpus_id, etc.)
  amount_remaining      INTEGER NOT NULL,       -- credits in pool, depletes by 1 per payout
  payout_interval_s     INTEGER NOT NULL,       -- seconds between payouts
  rotator_position      INTEGER NOT NULL DEFAULT 0,  -- cycles through qualifying providers
  status                TEXT NOT NULL DEFAULT 'active',  -- 'active' | 'exhausted' | 'cancelled'
  last_payout_at        TIMESTAMPTZ,
  created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE wire_incentive_pools ENABLE ROW LEVEL SECURITY;
GRANT ALL ON wire_incentive_pools TO service_role;
CREATE INDEX idx_incentive_pools_criteria ON wire_incentive_pools(criteria_type) WHERE status = 'active';
CREATE INDEX idx_incentive_pools_payout ON wire_incentive_pools(last_payout_at) WHERE status = 'active';
```

**Criteria types for compute market:**
- `model_availability` — keep model X loaded and serving
- `model_seeding` — download and load model X (first-to-load bonus)
- `compute_capacity` — maintain N available queue slots for model X

**Criteria types for storage market:**
- `document_hosting` — keep corpus X replicated
- `first_host` — first to host new document in corpus X (bonus payout)

**Criteria types for relay market:**
- `relay_capacity` — maintain N Mbps of relay bandwidth available

All use the same `wire_incentive_pools` table, same payout mechanism, same rotator arm. One primitive, all markets.

### C. Three Specialized Daemons (not one unified daemon)

The compute, storage, and relay markets each get their own daemon. They are simple, composable, and independently experimentable by the steward.

**Why three, not one:**
- Resources don't overlap: compute = GPU/VRAM, storage = disk, relay = bandwidth
- The steward can run experiments on each independently (change storage pricing while holding compute pricing constant)
- Each daemon is small enough for one agent to implement and one auditor to verify (Pillar 40)
- A bug or bad experiment in one daemon doesn't cascade to the others
- The steward IS the coordination layer — it adjusts each daemon's contributions independently

**Phase 6 update:** Daemon intelligence is per-daemon, not a unified optimizer. The steward composes them but doesn't entangle them. If cross-market resource allocation is needed (M-series unified memory: VRAM vs filesystem cache), the steward adjusts each daemon's allocation as separate experiments.

**Phase 7-9 update:** Sentinel and steward monitor all three daemons but each daemon has its own sentinel check chain and its own experimental surface. The steward's meta-coordination chain orchestrates across daemons.

### D. Phase 5 Reconceived: Quality via Existing Primitives

**Old Phase 5:** Wire dispatches blind review batches. Dedicated reviewer role. `wire_compute_review_batches` table.

**New Phase 5:** Quality enforcement through existing primitives:
1. **Challenges** (Pillar 24) — reactive, when someone suspects bad output. Existing infrastructure.
2. **Steward quality publications** — proactive, stewards naturally run comparison tests and publish structured analysis. Natural byproduct of the optimization loop.
3. **Timing-based anomaly detection** — passive, network-observed performance catches physically implausible responses.
4. **Clawback-funded enforcement** (Pillar 11) — the benefit of cheating funds the response.

**Removed:** `wire_compute_review_batches` table, review dispatch system, reviewer provider class. These are unnecessary — challenges + steward publications + anomaly detection cover quality without a dedicated review system.

**Why no review market:** Cheap compromised reviews are the exact attack vector. If review pricing is competitive, bad actors offer cheap reviews to get selected and then rubber-stamp everything. Quality enforcement must be integrity infrastructure (Wire-controlled), not a market function.

### E. Two-Hop Relay Privacy (Large Payloads)

For payloads above a threshold (contribution: `wire_proxy_payload_threshold_bytes`), route through two relay nodes instead of Wire-proxying:

```
Requester → Relay A → Relay B → Provider
```

**Properties:**
- No single non-provider node knows both endpoints (Relay A knows requester, Relay B knows provider, neither knows both)
- Each relay has plausible deniability (doesn't know its position in the chain)
- End-to-end encryption between requester and provider (relays see ciphertext only for compute; plaintext OK for storage since document bodies are public content)
- Relay fees: 1-2 credits per hop via rotator arm (76/2/2)
- Relay nodes = fifth provider class (just bandwidth, lowest hardware barrier)

**Size-based routing (contribution-driven threshold):**
- Small payloads: Wire-proxy (simple, Wire handles it)
- Large payloads: Two-hop relay (bandwidth-efficient, stronger privacy)

See companion doc: `docs/architecture/wire-market-privacy-tiers.md`

### F. Pillar 8 Update Required

Current Pillar 8 text refers to structural deflation and queries destroying credits. This is stale — the balancer pool and query governor changed the credit dynamics. The credit system serves to maximize utility of the network to its participants, and may be inflationary or deflationary depending on conditions.

**Action:** Update `wire-pillars.md` Pillar 8 text. Out of scope for this plan but noted as a dependency. The compute market's Wire subsidy (estimation risk absorption) is a credit creation mechanism that interacts with the broader credit economy.

### G. Renamed Shared Infrastructure

The `advance_market_rotator` function defined in the prerequisites migration is used by compute, storage, AND relay markets. Rename to `advance_market_rotator` to reflect its shared nature. Same for `market_rotator_recipient` → `market_rotator_recipient`. The rotator table becomes `wire_market_rotator` (not `wire_market_rotator`).

Update prerequisites migration accordingly.
