# Market Seed Contributions — V1

All YAML configurations needed to bootstrap the compute, storage, and relay markets. Each becomes a bundled contribution in `assets/bundled_contributions.json` with accompanying schema_definition, schema_annotation, and generation skill.

**Convention:** All numbers that could be Pillar 37 violations are explicitly marked as v1 seeds — supersedable by operator or network governance. The platform seeds first because someone must (DD-5).

---

## I. Node-Side Config Contributions

These live in the node's local `pyramid_config_contributions` table. Per-node, per-operator. The operator customizes via the generative config UI (intent → YAML → accept).

### 1. `compute_pricing` — Per-Model Compute Market Pricing

One per model the node offers. Defines rates, competitive strategy, queue discount curve, and reservation fee.

```yaml
schema_type: compute_pricing
model_id: llama-3.1-70b-instruct
provider_type: local                    # "local" | "bridge"

# Pricing mode
pricing_mode: competitive               # "fixed" | "competitive"
competitive_target: match_best          # "match_best" | "undercut_best" | "premium_over_best"
competitive_offset_bps: 0               # basis points relative to target

# Rates (credits per million tokens) — used when pricing_mode = "fixed"
# or as starting point when competitive mode hasn't resolved yet
rate_per_m_input: 500
rate_per_m_output: 800

# Bounds (competitive mode clamps to these)
floor_per_m_input: 100
floor_per_m_output: 150
ceiling_per_m_input: 5000
ceiling_per_m_output: 8000

# Reservation fee (per queue slot, non-refundable)
reservation_fee: 2

# Queue discount curve (integer basis points, Pillar 9)
# Maps queue depth to price multiplier. Deeper queue = cheaper.
queue_discount_curve:
  - depth: 0
    multiplier_bps: 10000               # 1.0x (full price)
  - depth: 3
    multiplier_bps: 8500                # 0.85x
  - depth: 8
    multiplier_bps: 6500                # 0.65x
  - depth: 15
    multiplier_bps: 4500                # 0.45x

# Queue limits
max_queue_depth: 20
```

### 2. `compute_capacity` — Compute Market Capacity Limits

One per node (global, not per-model). Defines how much of the node's resources the compute market can use.

```yaml
schema_type: compute_capacity

# Per-model queue limits (defaults — can be overridden per model in compute_pricing)
default_max_market_depth: 5             # max market jobs in any model's queue
default_max_total_depth: 20             # max total jobs (local + market) in any model's queue

# GPU concurrency
gpu_concurrency: 1                      # jobs processed simultaneously per model queue
                                        # default 1 (serial, most stable)

# Compute market enabled
enabled: false                          # must be explicitly enabled by operator
```

### 3. `compute_bridge` — Bridge Operator Configuration

Only needed for nodes that bridge cloud APIs to the network.

```yaml
schema_type: compute_bridge

enabled: false
provider: openrouter                    # "openrouter" | future: "direct_api"

# OpenRouter config
openrouter_base_url: https://openrouter.ai/api/v1

# Models to bridge (auto-detected from OpenRouter if empty)
model_allowlist: []                     # empty = bridge all available models

# Margin target (informational — helps operator set pricing)
target_margin_bps: 2000                 # target 20% margin above dollar cost
```

### 4. `storage_pricing` — Storage Market Pricing

One per node. Defines per-pull rate and competitive strategy.

```yaml
schema_type: storage_pricing

# Pricing mode
pricing_mode: competitive
competitive_target: match_best
competitive_offset_bps: 0

# Rate (credits per document pull)
rate_per_pull: 1                        # fixed rate when pricing_mode = "fixed"

# Bounds
floor_per_pull: 1                       # minimum 1 credit (smallest unit)
ceiling_per_pull: 20

# Storage market enabled
enabled: false
```

### 5. `storage_capacity` — Storage Market Capacity Limits

```yaml
schema_type: storage_capacity

# Disk allocation
storage_cap_gb: 10                      # max disk space for hosted documents

# Hosting behavior
auto_host_enabled: true                 # daemon auto-hosts best opportunities
auto_drop_enabled: true                 # daemon drops underperformers when near capacity
auto_drop_threshold_pct: 90             # drop underperformers when usage exceeds this %
```

### 6. `relay_pricing` — Relay Market Pricing

```yaml
schema_type: relay_pricing

# Pricing mode
pricing_mode: competitive
competitive_target: match_best
competitive_offset_bps: 0

# Rate (credits per relay hop)
rate_per_hop: 1

# Bounds
floor_per_hop: 1
ceiling_per_hop: 10

# Relay market enabled
enabled: false
```

### 7. `relay_capacity` — Relay Market Capacity Limits

```yaml
schema_type: relay_capacity

# Bandwidth allocation
max_concurrent_relays: 5                # max simultaneous relay streams
max_bandwidth_mbps: 50                  # self-limit bandwidth (0 = unlimited)
```

### 8. `privacy_policy` — Privacy Configuration

Controls relay usage and tunnel rotation for this node's OUTBOUND requests (when acting as requester).

```yaml
schema_type: privacy_policy

# Relay hops for outbound requests
default_relay_count: 0                  # relays for normal work (0 = direct with plausible deniability)
sensitive_relay_count: 2                # relays for sensitive builds (steward can escalate)
relay_count_range:
  min: 0
  max: 20

# Fan-out policy
max_jobs_per_provider: 10               # max calls to same provider in one build

# Tunnel rotation
tunnel_rotation_enabled: false          # disabled by default (operator enables when ready)
tunnel_rotation_interval_s: 3600        # rotate every hour when enabled
tunnel_drain_grace_s: 60               # keep old tunnel alive for draining
```

### 9. `model_loading_policy` — Pre-Steward Model Selection Heuristic

For Phases 1-5 (before steward). Simple rule for which models to load based on demand signals.

```yaml
schema_type: model_loading_policy

# Mode
mode: manual                            # "manual" | "demand_responsive"

# Demand responsive settings (Phase 6+)
# When mode = demand_responsive:
demand_threshold_jobs_per_hour: 5       # load model if unfilled demand exceeds this
unload_idle_hours: 24                   # unload model if no jobs for this long
max_models_loaded: 2                    # hardware-aware limit
```

---

## II. Wire-Side Economic Parameters

These are seeded as `wire_contributions` on the platform via migration. Type: `economic_parameter`. Global. Supersedable by governance.

### 10. Rotator Arm Configuration

```yaml
schema_type: economic_parameter
parameter_name: market_rotator_config
description: Rotator arm slot allocation for all market settlements

total_slots: 80
provider_slots: 76                      # 95%
wire_slots: 2                           # 2.5%
graph_fund_slots: 2                     # 2.5%
distribution: bjorklund                 # evenly spaced via Bjorklund algorithm
```

### 11. Deposit Configuration

```yaml
schema_type: economic_parameter
parameter_name: compute_deposit_config
description: How token deposits are calculated for compute jobs

deposit_percentage_bps: 10000           # 100% of estimated cost (10000 bps = 100%)
                                        # decreases as market matures and estimates improve
```

### 12. Default Output Estimate

```yaml
schema_type: economic_parameter
parameter_name: default_output_estimate
description: Fallback output token estimate when no network observations exist for a model

default_output_tokens: 500              # used by fill RPC when percentile_cont returns NULL
                                        # superseded by model-family-specific estimates as data accrues
```

### 13. Staleness Thresholds

```yaml
schema_type: economic_parameter
parameter_name: staleness_thresholds
description: How fresh queue/offer state must be for matching

queue_mirror_staleness_s: 120           # reject matches against mirrors older than 2 minutes
heartbeat_staleness_s: 300              # deactivate offers for nodes not seen in 5 minutes
```

### 14. Relay Minimum Performance

```yaml
schema_type: economic_parameter
parameter_name: relay_performance_floor
description: Minimum quality for relay participation

min_reliability_bps: 9000              # 90% success rate to remain active as relay
min_bandwidth_mbps: 1                  # minimum bandwidth to qualify
evaluation_window_days: 7              # measured over this period
```

---

## III. Wire-Side Incentive Pool Criteria Types

These define WHAT behaviors incentive pools can incentivize. Each is a contribution on the Wire (`schema_type: incentive_criteria`). Anyone can propose new criteria types — they're contributions, supersedable.

### 15. Model Availability Criteria

```yaml
schema_type: incentive_criteria
criteria_name: model_availability
description: Incentivize nodes to keep a specific model loaded and serving

required_params:
  - name: model_id
    type: string
    description: The model that must be loaded
  - name: min_providers
    type: integer
    description: Desired minimum number of providers serving this model

qualification_check: |
  Provider has the specified model loaded (appears in their queue state models_loaded)
  AND provider has an active compute offer for that model
  AND provider's node is online (heartbeat fresh)

payout_distribution: per_qualifying_provider
```

### 16. Document Hosting Criteria

```yaml
schema_type: incentive_criteria
criteria_name: document_hosting
description: Incentivize nodes to host documents from a specific corpus

required_params:
  - name: corpus_id
    type: uuid
    description: The corpus whose documents should be hosted
  - name: min_replicas
    type: integer
    description: Desired minimum replication level

qualification_check: |
  Provider hosts at least one document from the specified corpus
  (appears in wire_document_availability)
  AND provider's node is online

payout_distribution: document_rotator
payout_distribution_note: |
  Each payout tick, the document rotator selects one document from the corpus.
  The provider hosting that document receives the payout (via market rotator arm).
  Over N ticks, each document gets ~equal payouts. Providers hosting more documents
  receive proportionally more payouts.
```

### 17. Relay Capacity Criteria

```yaml
schema_type: incentive_criteria
criteria_name: relay_capacity
description: Incentivize nodes to maintain relay bandwidth

required_params:
  - name: min_bandwidth_mbps
    type: integer
    description: Minimum bandwidth the relay must offer
  - name: min_reliability_bps
    type: integer
    description: Minimum reliability in basis points (0-10000)

qualification_check: |
  Node has active relay offer
  AND observed bandwidth >= min_bandwidth_mbps
  AND observed reliability >= min_reliability_bps

payout_distribution: per_qualifying_provider
```

### 18. First Host Criteria

```yaml
schema_type: incentive_criteria
criteria_name: first_host
description: Bonus for first providers to host a new document or load a new model

required_params:
  - name: target_type
    type: string
    description: "'document' | 'model'"
  - name: target_id
    type: string
    description: Document ID or model ID
  - name: max_bonus_recipients
    type: integer
    description: How many providers get the bonus (first N to qualify)

qualification_check: |
  Provider is among the first N to host the document or load the model
  AND passes retention challenge (for documents) or serves a test job (for models)

payout_distribution: one_time_per_qualifier
payout_distribution_note: |
  Each qualifying provider gets one bonus payout from the pool.
  After max_bonus_recipients have been paid, this criteria deactivates for this target.
```

---

## IV. Sentinel / Steward Seeds (Phases 7-9)

The sentinel and steward are **action chains**, not config contributions with inline logic. They manifest in two modes (or any mix):

**Mode 1: External agent (webhook trigger).** The chain fires a webhook to wake an external autonomous agent (Claude Code, OpenClaw, etc.). The agent engages via CLI to run experiments, read metrics, make policy changes, publish analysis. The chain is trivially simple — the intelligence is external.

**Mode 2: Wire-native chain.** The chain itself contains the full loop — using Wire actions, skills, and templates. Each LLM step is fulfilled by the dispatch policy (local GPU, fleet, or compute market). The chain is self-contained.

The operator picks their mode via a `chain_assignment` contribution. The chains live in `chains/defaults/` and are themselves contributions (forkable, improvable, publishable on the Wire).

### 19. Sentinel Chain Assignment

```yaml
schema_type: chain_assignment
assignment_role: sentinel
chain_id: sentinel-native               # or: sentinel-external
trigger: timer
trigger_interval_s: 300
```

### 20. Steward Chain Assignment

```yaml
schema_type: chain_assignment
assignment_role: steward
chain_id: steward-native                # or: steward-external
trigger: escalation                     # fires when sentinel escalates
# The steward operates autonomously within experimental territory.
# It treats the operator as a boss (sets direction, territory) not a manager (approves each action).
# After each action period, it produces a status report for the operator:
# what happened, what changed, why, what it recommends next.
# The operator can redirect via natural language ("focus on compute revenue")
# or one-button actions ("undo that pricing change").
```

### 21. Sentinel Chain — External Agent Mode

File: `chains/defaults/sentinel-external.yaml`

```yaml
name: sentinel-external
description: Wake an external agent to perform sentinel duties via CLI
content_type: sentinel

steps:
  - name: wake_agent
    mode: single
    type: mechanical
    recipe: webhook_dispatch
    params:
      url: $config.sentinel_webhook_url
      payload:
        role: sentinel
        node_id: $node.id
        metrics_endpoint: $node.api_url
        period_since_last_check_s: $trigger.elapsed_s
```

### 22. Sentinel Chain — Wire-Native Mode

File: `chains/defaults/sentinel-native.yaml`

```yaml
name: sentinel-native
description: Self-contained sentinel using Wire actions, skills, and templates
content_type: sentinel

steps:
  - name: gather_metrics
    mode: single
    type: mechanical
    recipe: read_node_metrics

  - name: assess_health
    mode: single
    type: llm
    skill: sentinel-health-assessment
    template: sentinel-report
    input: $gather_metrics.output

  - name: auto_adjust
    mode: single
    type: mechanical
    recipe: apply_config_adjustment
    when: $assess_health.output.adjustment_needed
    input: $assess_health.output.adjustment

  - name: escalate_to_steward
    mode: single
    type: mechanical
    recipe: trigger_chain
    when: $assess_health.output.needs_judgment
    params:
      chain_role: steward
```

### 23. Steward Chain — External Agent Mode

The external agent subscribes to the Wire's subscribable webhook system (credit cost per delivery, batching built in). The ~48 node management functions are available via the standard authenticated API. The agent receives batched updates and acts autonomously via API calls — same as the native steward but with external intelligence.

File: `chains/defaults/steward-external.yaml`

```yaml
name: steward-external
description: Wake an external agent via subscribable webhook system
content_type: steward

steps:
  - name: wake_agent
    mode: single
    type: mechanical
    recipe: webhook_dispatch
    params:
      url: $config.steward_webhook_url
      payload:
        role: steward
        node_id: $node.id
        escalation_reason: $trigger.escalation_reason
        metrics_endpoint: $node.api_url
        # The external agent (Claude Code, OpenClaw, etc.) uses the standard
        # Wire API to read metrics, supersede contributions, manage offers.
        # Webhook subscriptions provide batched event delivery:
        #   - queue_state_changed
        #   - market_job_completed
        #   - settlement_processed
        #   - sentinel_escalation
        #   - heartbeat_market_data
        #   - performance_profile_updated
        # Each delivery costs 1 credit (economic gate, prevents spam).
        # Bundle window configurable (e.g., 30 seconds).
```

### 24. Steward Chain — Wire-Native Mode

File: `chains/defaults/steward-native.yaml`

```yaml
name: steward-native
description: Full experiment loop using Wire intelligence primitives
content_type: steward

steps:
  - name: observe
    mode: single
    type: mechanical
    recipe: gather_experiment_context

  - name: hypothesize
    mode: single
    type: llm
    skill: steward-experiment-design
    template: experiment-proposal
    input: $observe.output

  - name: apply_change
    mode: single
    type: mechanical
    recipe: supersede_contribution
    input: $hypothesize.output.change

  - name: wait_for_measurement
    mode: single
    type: mechanical
    recipe: sleep
    params:
      duration_s: $hypothesize.output.measurement_window_s

  - name: measure
    mode: single
    type: mechanical
    recipe: gather_experiment_context

  - name: decide
    mode: single
    type: llm
    skill: steward-experiment-evaluation
    input:
      before: $observe.output
      after: $measure.output
      hypothesis: $hypothesize.output

  - name: keep_or_revert
    mode: single
    type: mechanical
    recipe: conditional_revert
    when: $decide.output.action == "revert"
    input: $apply_change.output.previous_contribution_id

  - name: publish
    mode: single
    type: mechanical
    recipe: publish_experiment_result
    when: $decide.output.action == "keep" and $decide.output.improvement_bps > 500
    input: $decide.output
```

---

## V. Schema Reflection — What Does This Require?

### New Schema Types Needed (node-side dispatcher branches)

| Schema Type | Needs Operational Table? | Needs Reload Hook? |
|---|---|---|
| `compute_pricing` | Yes — `pyramid_compute_pricing` (per model) | Yes — refresh Wire offer |
| `compute_capacity` | Yes — `pyramid_compute_capacity` | Yes — adjust queue limits |
| `compute_bridge` | Yes — `pyramid_compute_bridge` | Yes — toggle bridge mode |
| `storage_pricing` | Yes — `pyramid_storage_pricing` | Yes — refresh Wire offer |
| `storage_capacity` | Existing — `pyramid_local_mode_state` extended? Or new | Yes — adjust storage daemon |
| `relay_pricing` | Yes — `pyramid_relay_pricing` | Yes — refresh Wire offer |
| `relay_capacity` | Yes — `pyramid_relay_capacity` | Yes — adjust relay limits |
| `privacy_policy` | Yes — `pyramid_privacy_policy` | No — read at dispatch time |
| `model_loading_policy` | Yes — `pyramid_model_loading_policy` | Yes — trigger model evaluation |
| `relay_pricing` | Yes — `pyramid_relay_pricing` | Yes — refresh Wire offer |
| `relay_capacity` | Yes — `pyramid_relay_capacity` | Yes — adjust relay limits |
| (sentinel/steward) | Via `chain_assignment` — existing type, no new dispatcher branch | Existing chain executor handles |

### New Schema Types Needed (Wire-side)

| Schema Type | Table | Purpose |
|---|---|---|
| `economic_parameter` | Existing `wire_contributions` | Global economic params |
| `incentive_pool` | New `wire_incentive_pools` (defined in compute plan Addendum B) | Pool state + payout tracking |
| `incentive_criteria` | `wire_contributions` (just metadata) | Criteria type definitions |
| `hosting_grant` | New `wire_hosting_grants` (defined in storage plan §IV) | Grant state + document rotator |

### What Changes to the Existing System

1. **`sync_config_to_operational` needs 9 new dispatcher branches** (one per node-side schema type). Each is simple: deserialize YAML → upsert operational table → optional reload hook.

2. **`bundled_contributions.json` gains ~36 entries** (9 node-side schema types × 4 entries each: seed, schema_definition, schema_annotation, generation skill). Sentinel/steward use existing `chain_assignment` type (no new bundled entries for them beyond the chain YAML files).

3. **`db.rs` gains 9 new YAML structs + operational tables + upsert/load functions.** (compute_pricing, compute_capacity, compute_bridge, storage_pricing, storage_capacity, relay_pricing, relay_capacity, privacy_policy, model_loading_policy). All follow the existing pattern exactly.

4. **The Wire needs a new migration** to seed the economic parameters (10-14 above) and incentive criteria types (15-18 above) as `wire_contributions`.

5. **No changes to the contribution store schema itself** — `pyramid_config_contributions` handles all new types. The `schema_type` column is freeform TEXT, not an enum. New types just work.

6. **No changes to the Wire contribution store** — `wire_contributions` already supports arbitrary types via the `type` and `structured_data` columns.

7. **Sentinel and steward use `chain_assignment` (existing type) + chain YAML files in `chains/defaults/`.** No new schema types needed for them. Two execution modes (external webhook, wire-native) are just different chains assigned to the same role. The chain executor handles both.

### What This Tells Us About the Plan

**Good news:** The existing config contribution system is fully extensible. New schema types are just new YAML structs + dispatcher branches. No schema changes needed. No migration to the contribution store itself. Sentinel/steward reuse existing `chain_assignment` type — no new types for them.

**One concern:** The `sync_config_to_operational` dispatcher is already a large match statement (15+ branches). Adding 7 new market types makes it 22+. Phase 1 should refactor to a registry pattern (HashMap of schema_type → handler function) rather than a growing match block. Cleanup, not design change.

### New Mechanical Recipes Needed for Sentinel/Steward Chains

The sentinel and steward chains reference mechanical recipes that don't exist yet:

| Recipe | What it does | Phase |
|---|---|---|
| `webhook_dispatch` | POST to a URL with a JSON payload | Phase 7 |
| `read_node_metrics` | Reads queue state, earnings, performance from local APIs | Phase 7 |
| `apply_config_adjustment` | Supersedes a config contribution with new values | Phase 7 |
| `trigger_chain` | Fires another chain by role (e.g., wake the steward) | Phase 7 |
| `gather_experiment_context` | Reads market data, history, current configs for steward | Phase 8 |
| `supersede_contribution` | Creates a new contribution superseding an existing one | Phase 8 |
| `sleep` | Waits for a duration (measurement window) | Phase 8 |
| `conditional_revert` | Reverts a supersession if the experiment failed | Phase 8 |
| `publish_experiment_result` | Publishes experiment results to the Wire | Phase 8 |
| `generate_status_report` | Produces steward status report for the operator (what happened, what changed, why) | Phase 7 |
| `fleet_announce` | POST to all fleet peers' tunnels with current state (models, queue, tunnel URL) | Phase 2 |

These are all straightforward extensions of the existing mechanical dispatch system in `chain_dispatch.rs`. Each is a new branch in the mechanical handler.

### Node Endpoints Added by Market System

| Endpoint | Purpose | Phase |
|---|---|---|
| `/v1/compute/job-dispatch` | Receive compute jobs from Wire (via relay chain) | Phase 2 |
| `/v1/compute/result-delivery` | Receive completed results from Wire (webhook to requester) | Phase 3 |
| `/v1/compute/fleet-dispatch` | Fleet-internal compute dispatch (same operator, fleet JWT) | Phase 2 |
| `/v1/relay/forward` | Relay forwarding endpoint (stream in, stream out) | Phase R1 |
| `/v1/fleet/announce` | Fleet peer announcement (direct peer-to-peer, instant discovery) | Phase 2 |
