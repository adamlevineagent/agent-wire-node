# Phase 4: Bridge Operations

**What ships:** Nodes bridge cloud models (OpenRouter) to the compute market. Bridge is a provider type — receives market jobs, dispatches to OpenRouter, returns results for credits. Dual-currency settlement (credits on Wire, dollars on OpenRouter).

**Prerequisites:** Phase 3 (full settlement loop with ACK+async result delivery), bridge-dedicated OpenRouter API key provisioned via Management API

---

## I. Overview

Bridge mode turns a Wire node into a relay to cloud inference. The node receives market jobs from the Wire exchange (paid in credits), dispatches them to OpenRouter (paid in dollars), and returns results. The operator earns credits, pays dollars, and profits on the spread.

This is substantially more complex than the original 18-line sketch because it introduces the first EXTERNAL dependency into the settlement path. Every other market provider type (local GPU, fleet) has a single currency surface. Bridge has two:

1. **Credit settlement (Wire side):** Requester pays credits to provider via the existing Phase 3 settlement loop. This side works identically to local GPU settlement.
2. **Dollar settlement (OpenRouter side):** Bridge node pays real dollars per API call. This cost is invisible to the Wire. The operator absorbs it.

The dual settlement surface creates failure modes that don't exist for local GPU providers: Wire settlement can succeed while OpenRouter billing fails (operator profits without providing value), or OpenRouter can succeed while Wire settlement fails (operator loses money). The error classification table in Section III is load-bearing for handling every combination.

Bridge also degrades privacy. Local GPU providers see the prompt but no one else does. Bridge providers see the prompt AND forward it to OpenRouter AND OpenRouter forwards it to the upstream model provider. The privacy disclosure in Section IV.B is mandatory — bridge offers cannot masquerade as standard privacy tier.

---

## II. Dollar-to-Credit Conversion Mechanism

The core economic problem: a bridge operator receives credits but pays dollars. The credit floor for bridge offers must mechanically derive from OpenRouter's current dollar cost. Hand-waving this as "operators set their own price" fails because OpenRouter prices change without notice (model deprecations, price cuts, new model launches). A bridge offer priced below dollar cost burns operator money.

### A. Reading OpenRouter Cost-Per-Token

OpenRouter publishes model pricing via `GET /api/v1/models`. Each model entry includes:

```
{
  "id": "meta-llama/llama-3.1-70b-instruct",
  "pricing": {
    "prompt": "0.0000003",    // USD per token (input)
    "completion": "0.0000004" // USD per token (output)
  }
}
```

The bridge daemon reads this endpoint. Caching policy:

- **Refresh interval:** Governed by `model_refresh_interval_s` field on the `compute_bridge` contribution (operator-controlled, not hardcoded). Initial seed value: 300s.
- **Cache invalidation:** On any OpenRouter 404 for a model that was previously available (model deprecated).
- **Storage:** In-memory `HashMap<String, OpenRouterModelPricing>` on the bridge daemon state. Not persisted to disk (re-fetched on restart).

### B. Dollar Floor Derivation

Given:
- OpenRouter cost: `$P_input` per token (input), `$P_output` per token (output)
- Credit/dollar rate: `R` credits per dollar (from `economic_parameter` contribution, parameter_name: `credit_retail_rate`)

The credit floor per million tokens:

```
floor_per_m_input  = ceil(P_input  * 1_000_000 * R)
floor_per_m_output = ceil(P_output * 1_000_000 * R)
```

**Example:** OpenRouter charges $0.30/M input for llama-3.1-70b. Credit/dollar rate is 10,000 credits/$1.

```
floor_per_m_input = ceil(0.30 * 10000) = 3000 credits/M input
```

The operator must price above this floor or lose money on every call. The `ceil()` ensures no sub-credit rounding losses.

### C. Floor Auto-Adjustment

When the bridge daemon's periodic model refresh detects a pricing change:

1. Recalculate floor from new OpenRouter price
2. Compare against current Wire offer rate
3. If current rate < new floor: immediately update Wire offer to `new_floor + margin`
4. If current rate >= new floor: no action (operator's margin increased, their choice to adjust)
5. Emit `bridge_floor_adjusted` chronicle event with old floor, new floor, and action taken

When the credit/dollar rate changes (supersession of `credit_retail_rate` economic_parameter):

1. Recalculate all bridge floors from cached OpenRouter prices using new rate
2. Same comparison and adjustment logic as above
3. Emit chronicle event for each affected model

### D. Competitive Auto-Pricing for Bridge

Bridge offers use the same competitive pricing schema from Addendum A, but with floor derivation from dollar cost:

```yaml
# compute_pricing contribution for a bridge offer
model: meta-llama/llama-3.1-70b-instruct
provider_type: bridge
pricing_mode: competitive
competitive_target: match_best
competitive_offset_bps: 0
# Floor is auto-derived from dollar cost — these values are COMPUTED, not operator-set
floor_per_m_input: 3000     # auto: ceil($0.30 * 10000)
floor_per_m_output: 4000    # auto: ceil($0.40 * 10000)
# Ceiling remains operator-set
ceiling_per_m_input: 20000
ceiling_per_m_output: 30000
# Fixed rate (used when pricing_mode = "fixed")
rate_per_m_input: 5000
rate_per_m_output: 8000
reservation_fee: 2
```

The daemon resolves: `effective_rate = clamp(apply_strategy(best_market_rate, strategy), dollar_derived_floor, ceiling)`. The Wire only sees the resolved number. The dollar floor is invisible to the Wire — it's local operator protection, not a Wire concept.

**Key distinction from local GPU pricing:** Local GPU operators set floor manually (covers electricity). Bridge operators get floor computed automatically (covers dollar API cost). The pricing_mode and competitive strategy work identically; only the floor derivation differs.

---

## III. Error Classification Table

Every OpenRouter HTTP response must map to a Wire job state transition and an offer state change. This table is exhaustive — implementers handle every case.

| OpenRouter HTTP | Wire Job State | Offer State | Retry? | Failure Reason Enum | Notes |
|----------------|---------------|-------------|--------|-------------------|-------|
| 200 | completed → settle | unchanged | no | n/a | Happy path. Extract `actual_cost_usd` from response. |
| 400 (bad request) | failed | unchanged | no | `upstream_bad_request` | Malformed prompt relay. Bug in bridge, not transient. |
| 400 (context exceeded) | failed | unchanged | no | `context_exceeded` | Requester should retry with shorter prompt or different model. |
| 401 (unauthorized) | failed | ALL bridge offers suspended | no | `upstream_auth_failure` | API key revoked or expired. Operator must re-provision. |
| 402 (insufficient funds) | failed | ALL bridge offers suspended | no | `upstream_funds_exhausted` | OpenRouter credits depleted. Operator must add funds. |
| 403 (forbidden) | failed | THIS model's offer suspended | no | `upstream_forbidden` | Model access restricted (gated model, ToS violation). |
| 404 (model not found) | failed | THIS model's offer suspended | no | `upstream_model_removed` | Model deprecated. Trigger model lifecycle refresh. |
| 408 (request timeout) | failed | unchanged | no | `upstream_timeout` | Request took too long to start processing. |
| 429 (rate limited) | retry internally (max 3 attempts, exponential backoff starting at 2s) → fail if exhausted | unchanged | yes, bounded | `upstream_rate_limited` | Transient. If rate limit is sustained (3 consecutive 429s in 60s), consider reducing offer queue depth. |
| 500 (server error) | retry internally (max 2 attempts) → fail if exhausted | unchanged | yes, bounded | `upstream_server_error` | Transient. OpenRouter internal issue. |
| 502 (bad gateway) | retry internally (max 2 attempts) → fail if exhausted | unchanged | yes, bounded | `upstream_bad_gateway` | Upstream model provider down. |
| 503 (model unavailable) | failed | THIS model's offer suspended | no | `upstream_model_unavailable` | Model temporarily or permanently unavailable. Trigger lifecycle check. |
| 504 (gateway timeout) | failed | unchanged | no | `upstream_gateway_timeout` | OpenRouter-to-provider timeout. ACK+async should prevent for bridge-originated calls, but OpenRouter itself may timeout. |

### Failure Reason Enum

Add `failure_reason` to the `fail_compute_job` RPC. This is a new TEXT column on `wire_compute_jobs`:

```sql
ALTER TABLE wire_compute_jobs ADD COLUMN failure_reason TEXT;
-- Values from the enum above, plus existing non-bridge reasons:
-- 'upstream_bad_request', 'context_exceeded', 'upstream_auth_failure',
-- 'upstream_funds_exhausted', 'upstream_forbidden', 'upstream_model_removed',
-- 'upstream_timeout', 'upstream_rate_limited', 'upstream_server_error',
-- 'upstream_bad_gateway', 'upstream_model_unavailable', 'upstream_gateway_timeout',
-- 'provider_timeout' (existing, for local GPU timeout),
-- 'provider_error' (existing, for local GPU crash),
-- 'provider_cancelled' (existing)
```

### Offer Suspension Logic

Two severity levels:

1. **Global suspension (401, 402):** ALL bridge offers for this node go `status='suspended'`. Bridge mode deactivated. Requires operator intervention to re-enable.
2. **Per-model suspension (403, 404, 503):** Only the affected model's offer goes `status='suspended'` with `suspension_reason`. Other models continue serving.

Suspension emits `bridge_offers_suspended` chronicle event with scope ('global' | 'model'), affected offer IDs, and trigger error.

### Double-Settlement Failure Handling

| Wire Settlement | OpenRouter Billing | Outcome | Action |
|---|---|---|---|
| Success | Success | Normal | Record dollar cost, credit revenue, calculate margin |
| Success | Failure (free inference) | Operator windfall | Log anomaly. No clawback — OpenRouter's billing is their problem. Record $0 cost. |
| Failure | Success | Operator loss | This is the dangerous case. Mitigated by: (1) Bridge dispatches to OpenRouter ONLY after Wire settlement succeeds — never speculatively. (2) If Wire settlement RPC fails after OpenRouter call returns, the bridge records a `bridge_unrecoverable_loss` chronicle event with the dollar amount. The operator sees this in the economics view. The Wire cannot retroactively settle. |
| Failure | Failure | No harm | Job failed end-to-end. Normal failure path. |

**Settlement ordering is load-bearing:** The bridge MUST NOT call OpenRouter until the Wire job is in `executing` state with deposit locked. The dispatch sequence is:

1. Wire `fill_compute_job` succeeds (deposit locked)
2. Wire transitions job to `executing`
3. Bridge dispatches to OpenRouter
4. OpenRouter returns result
5. Bridge calls Wire `settle_compute_job` with actual token counts
6. Wire settles credits

If step 5 fails, the bridge has already paid OpenRouter (step 3-4 succeeded). This is the unrecoverable loss case. It should be rare (Wire RPC failure during settlement) and the chronicle event ensures the operator can audit it.

---

## IV. Wire Workstream

### A. Bridge-Specific Offer Fields

The `wire_compute_offers` table already has `provider_type TEXT NOT NULL DEFAULT 'local'` with values `'local' | 'bridge'` and a UNIQUE constraint on `(node_id, model_id, provider_type)`. This means the same model can have both a local GPU offer and a bridge offer from the same node.

New columns for bridge visibility (optional, operator-facing, NOT used by matching):

```sql
ALTER TABLE wire_compute_offers
  ADD COLUMN dollar_cost_floor_input  INTEGER,  -- floor credits/M derived from dollar cost (bridge only)
  ADD COLUMN dollar_cost_floor_output INTEGER;  -- floor credits/M derived from dollar cost (bridge only)
```

These are informational — the Wire doesn't enforce floor pricing. The node-side bridge daemon enforces floors locally. The Wire columns let operators see floor data in the market surface.

### B. Privacy Disclosure

Bridge offers MUST carry `privacy_capabilities: '{cloud_relay}'` — NOT `'{standard}'`. The `cloud_relay` value indicates:

- Prompt leaves the node (sent to OpenRouter API)
- Prompt is processed by external infrastructure (OpenRouter + upstream model provider)
- Prompt may be logged by external parties (depending on OpenRouter data policy and model provider policy)
- This is fundamentally different from local GPU inference where the prompt never leaves the node

**Migration:** Add `'cloud_relay'` to the CHECK constraint or validation on `privacy_capabilities`:

```sql
-- No actual CHECK exists (it's a TEXT[] column), but the matching RPC must validate.
-- In match_compute_job: if requester's dispatch_policy has bridge_allowed = false,
-- skip offers where provider_type = 'bridge' OR 'cloud_relay' = ANY(privacy_capabilities)
```

**Market surface disclosure:** When browsing providers in the market, bridge providers are clearly marked:

- Tag: "Cloud Relay" badge on the offer row
- Tooltip: "This provider relays inference through OpenRouter. Your prompt leaves the Wire network."
- Sort/filter: Requester can filter bridge providers in/out

**Dispatch policy extension:** Add `bridge_allowed` field to the requester's `dispatch_policy` contribution:

```yaml
# In dispatch_policy contribution
bridge_policy:
  allowed: true          # false = never match to bridge providers
  max_relay_depth: 0     # bridge + relay chain depth (bridge alone = 0 relays)
```

The matching RPC respects this: if `bridge_allowed = false`, offers with `provider_type = 'bridge'` are excluded from candidate set.

---

## V. Node Workstream

### A. Bridge Mode in compute_market.rs

Bridge dispatch is a special case of the provider-side job handler. When a market job arrives at a bridge node:

1. **Receive job** via `/v1/compute/job-dispatch` (same endpoint as local GPU market jobs)
2. **Check provider_type** on the matched offer. If `'bridge'`: route to bridge dispatch handler instead of local compute queue.
3. **Bridge dispatch handler:**
   - Extract prompt from the job payload (received via relay chain or Wire-proxied path)
   - Build OpenRouter API request with bridge-dedicated API key
   - Set `trace.metadata` fields for webhook correlation:
     ```json
     {
       "metadata": {
         "wire_job_id": "<job_id>",
         "wire_slug": "compute-market-bridge",
         "wire_step_name": "<job_id>"
       }
     }
     ```
   - Call OpenRouter `/api/v1/chat/completions`
   - Extract `actual_cost_usd` from response body (`usage.cost` field)
   - Map response to Wire settlement metadata (token counts, finish reason, latency)
   - Call Wire `settle_compute_job` RPC with actual token counts
   - Record dollar cost and credit revenue in local bridge economics ledger
   - Handle all error codes per classification table (Section III)

### B. Bridge-Dedicated API Key

The audit found that sharing one OpenRouter API key between personal builds and bridge jobs causes rate limit exhaustion — bridge traffic blocks the operator's own builds.

**Key provisioning options (ordered by preference):**

1. **Programmatic provisioning via Management API (preferred):**
   - On bridge activation, the Wire server calls `POST /api/v1/keys` with:
     - `name`: `"wire-bridge-{node_handle}"`
     - `limit`: operator-configured spend cap (from `compute_bridge` contribution)
     - `limit_reset`: `"monthly"` (default)
   - The new key is returned to the node and stored in `credentials.rs` alongside the personal key
   - Both keys charge the same OpenRouter account but have independent server-side rate limits
   - Wire configures a separate broadcast webhook destination filtered to the bridge key

2. **Manual separate key (fallback if Management API unavailable):**
   - BridgeConfigPanel includes a field for a bridge-dedicated OpenRouter API key
   - Operator creates the key manually on OpenRouter dashboard
   - Stored separately from the personal key in `credentials.rs`
   - Validation: on save, test the key with a lightweight `/api/v1/models` call

**Key isolation in code:**

```rust
// In LlmConfig or bridge state:
pub struct BridgeKeyConfig {
    /// Dedicated API key for bridge jobs. Never used for personal builds.
    pub bridge_api_key: String,
    /// Key hash for webhook correlation (first 8 chars of SHA-256).
    pub bridge_key_hash: String,
    /// Spend limit (informational — enforced by OpenRouter server-side).
    pub spend_limit_usd: Option<f64>,
}
```

The bridge dispatch handler reads `bridge_api_key` from bridge config, NOT from the primary `LlmConfig.openrouter_api_key`. The personal key path in `build_call_provider()` is untouched.

### C. Model Lifecycle Management

The audit found that auto-detected models deprecated on OpenRouter leave stale Wire offers active. The bridge daemon must actively manage model availability.

**On bridge activation:**

1. Query OpenRouter `GET /api/v1/models` for full model catalog
2. Filter by operator's `model_allowlist` (from `compute_bridge` contribution)
3. For each allowed + available model:
   - Calculate credit floor from OpenRouter pricing
   - Create Wire offer via `create_compute_offer` RPC with `provider_type='bridge'`, `privacy_capabilities='{cloud_relay}'`
4. Store model→pricing mapping in bridge daemon state

**Periodic refresh (every `model_refresh_interval_s`):**

1. Re-query `/api/v1/models`
2. Diff against current offer set:

   | Diff Result | Action |
   |---|---|
   | Model in allowlist + available + no offer exists | Create new offer |
   | Model in allowlist + available + offer exists + price changed | Recalculate floor, update offer if rate < new floor |
   | Model in allowlist + available + offer exists + price unchanged | No action |
   | Model was available + now unavailable | Set offer `status='suspended'`, `suspension_reason='upstream_model_unavailable'` |
   | Model was unavailable + now available | Reactivate offer (set `status='active'`), recalculate floor |
   | Model removed from allowlist | Set offer `status='inactive'` |
   | Model added to allowlist + available | Create new offer |

3. Emit `bridge_model_lifecycle` chronicle event for any changes

**Rate-limit-safe refresh:** The `/api/v1/models` endpoint is public and not rate-limited per OpenRouter docs. But as a precaution, the bridge daemon should back off exponentially if it receives 429 on the models endpoint (starting at 60s, max 3600s).

### D. Fleet vs Bridge Dispatch Ordering

The audit found that fleet-first routing could route an operator's own builds through their own bridge, paying OpenRouter dollars for inference that could have been served by their local GPU for free.

**The fix:** Fleet dispatch must distinguish local-GPU fleet nodes from bridge fleet nodes. The dispatch order becomes:

```
cache → route resolution → fleet-local-GPU → fleet-bridge → wire-compute-market → openrouter-personal
```

Implementation:

1. **Add `provider_type` to FleetPeer:**
   ```rust
   pub struct FleetPeer {
       // ... existing fields ...
       /// Provider types available at this peer.
       /// "local" = local GPU, "bridge" = cloud relay.
       /// A peer can be both (has local GPU AND runs bridge mode).
       #[serde(default)]
       pub provider_types: Vec<String>,  // ["local"], ["bridge"], ["local", "bridge"]
   }
   ```

2. **Fleet announce includes provider_types:** The fleet peer announcement payload (from heartbeat and direct announce) carries `provider_types` so dispatching nodes know which peers have local GPU vs bridge.

3. **Dispatch logic in llm.rs:**
   ```
   // Current: fleet dispatch checks serving_rules only
   // New: fleet dispatch splits into two passes:
   //   Pass 1: fleet peers with provider_types containing "local"
   //   Pass 2: fleet peers with provider_types containing "bridge" ONLY
   //           (and only when local fleet is exhausted AND bridge is cheaper than market)
   ```

4. **Same-operator bridge is last resort, not first choice:** If a fleet peer only has bridge capability (no local GPU), it's treated as more expensive than a same-operator local GPU peer. The operator pays nothing for local fleet dispatch but pays OpenRouter dollars for bridge fleet dispatch.

### E. OpenRouter Webhook Correlation

The existing `openrouter_webhook.rs` module correlates broadcast traces against `pyramid_cost_log` rows using `(slug, step_name, model)` as fallback when `generation_id` is missing.

Bridge jobs need a distinct StepContext so the correlator can distinguish bridge-for-market traces from personal build traces:

```rust
// Bridge job StepContext:
StepContext {
    slug: "compute-market-bridge".into(),
    build_id: None,  // bridge jobs are not builds
    step_name: job_id.clone(),  // unique per job
    depth: None,
    chunk_index: None,
    chain_id: None,
    force_fresh: false,
    event_bus: Some(event_bus.clone()),
}
```

**Correlation flow for bridge jobs:**

1. Bridge dispatch sets `trace.metadata.wire_slug = "compute-market-bridge"` and `trace.metadata.wire_step_name = <job_id>` in the OpenRouter request
2. `pyramid_cost_log` row created at dispatch time with `slug = "compute-market-bridge"`, `step_name = <job_id>`
3. Broadcast webhook arrives → correlator matches by `(slug="compute-market-bridge", step_name=<job_id>, model=<model_id>)`
4. `actual_cost_usd` from the synchronous response body is the primary cost source (available immediately, per-call)
5. Broadcast webhook serves as reconciliation/verification — if broadcast cost diverges from synchronous cost, the discrepancy detection in `openrouter_webhook.rs` fires

**Cost log entry for bridge jobs:**

```rust
// In pyramid_cost_log for a bridge job:
CostLogEntry {
    slug: "compute-market-bridge",
    build_id: None,
    step_name: job_id,
    model: model_id,
    actual_cost_usd: response.usage.cost,  // from OpenRouter response
    // ... standard fields ...
}
```

### F. DADBEAR Integration

Bridge jobs are DADBEAR work items per Theme 1 of the audit. The bridge daemon creates work items for received market jobs and routes them through DADBEAR's preview gate.

**Work item integration:**

```
Source: "bridge"
Semantic path: "bridge/{model_id}/{job_id}"
Example: "bridge/meta-llama-llama-3.1-70b-instruct/cm-job-abc123"
```

**DADBEAR lifecycle for a bridge job:**

1. **Observe:** Market job received → `dadbear_observation_events` row with `source = "bridge"`, `event_type = "job_received"`
2. **Compile:** Compiler produces work item with semantic path ID
3. **Preview:** Preview gate evaluates:
   - Dollar cost estimate (from cached OpenRouter pricing × estimated input tokens)
   - Credit revenue estimate (from matched rate × estimated tokens)
   - Margin check: if estimated dollar cost > estimated credit revenue → `cost_limit` hold
   - Budget check: if operator's remaining OpenRouter spend limit is below threshold → `breaker` hold
4. **Dispatch:** If preview approves, work item dispatched → bridge calls OpenRouter
5. **Apply:** Result received → work item completed, chronicle events emitted, settlement initiated

**Hold on negative margin:**

If the preview gate detects that the dollar cost would exceed credit revenue (negative margin), it places a `cost_limit` hold. This can happen when:
- OpenRouter raised prices since the offer was created but before the floor auto-adjusted
- The matched rate includes a queue discount that drops below dollar cost

The hold blocks dispatch. The bridge daemon's next model refresh cycle recalculates floors and updates offers. Once the floor is corrected, future jobs match at viable rates.

### G. Chronicle Events

Four new event source values for `pyramid_compute_events`:

| Event | Source | Fields | When |
|---|---|---|---|
| `bridge_dispatched` | `bridge` | job_id, model_id, openrouter_request_id, estimated_cost_usd | Bridge sends request to OpenRouter |
| `bridge_returned` | `bridge` | job_id, model_id, actual_cost_usd, prompt_tokens, completion_tokens, latency_ms, finish_reason | OpenRouter returns successful response |
| `bridge_failed` | `bridge` | job_id, model_id, http_status, failure_reason (enum value), retry_attempt, retries_exhausted | OpenRouter returns error |
| `bridge_cost_recorded` | `bridge` | job_id, model_id, dollar_cost_usd, credit_revenue, margin_credits, margin_pct_bps | After settlement, records the dual-currency economics |
| `bridge_floor_adjusted` | `bridge` | model_id, old_floor_input, new_floor_input, old_floor_output, new_floor_output, trigger ("price_change" or "rate_change") | Dollar floor recalculated |
| `bridge_offers_suspended` | `bridge` | scope ("global" or "model"), affected_offer_ids, trigger_error, trigger_http_status | Offers suspended due to error |
| `bridge_model_lifecycle` | `bridge` | model_id, action ("created", "suspended", "reactivated", "deactivated"), reason | Model availability change |

All events carry `work_item_id` and `attempt_id` for DADBEAR correlation (columns already exist on `pyramid_compute_events`).

---

## VI. Contribution Schemas

### A. compute_bridge

This contribution type was listed in the plan's contribution types table but never defined. Full schema:

```yaml
# Bridge configuration (schema_type: compute_bridge)
# One per node. Supersedable — new contribution replaces prior.

# Master enable gate
enabled: true

# API key reference (hash, not plaintext — actual key in local credentials store)
openrouter_key_id: "bridge-dedicated-key-hash"

# Model allowlist — only these models will have bridge offers created.
# Uses OpenRouter model IDs (e.g., "meta-llama/llama-3.1-70b-instruct").
# Empty list = no models offered (bridge enabled but dormant).
model_allowlist:
  - "meta-llama/llama-3.1-70b-instruct"
  - "anthropic/claude-3.5-sonnet"
  - "google/gemini-2.0-flash-001"

# How often to refresh model availability from OpenRouter (seconds).
# Governs both availability checks and pricing updates.
model_refresh_interval_s: 300

# Margin strategy for bridge offers.
# "fixed_margin" — static margin over dollar cost.
# "competitive" — dynamic pricing against market (from Addendum A), with dollar-derived floor.
margin_mode: competitive

# Fixed margin mode settings:
margin_bps: 1500                  # 15% margin over dollar cost floor

# Competitive mode settings (from Addendum A):
competitive_target: match_best    # "match_best" | "undercut_best" | "premium_over_best"
competitive_offset_bps: 0         # basis points relative to target

# Per-model ceiling overrides (optional). Default ceiling from compute_pricing.
ceiling_overrides:
  "anthropic/claude-3.5-sonnet":
    ceiling_per_m_input: 50000
    ceiling_per_m_output: 80000

# Spend safety
monthly_dollar_limit: 100.00      # hard cap on OpenRouter spend for bridge jobs (USD)
pause_on_negative_margin: true    # auto-pause bridge if any model shows negative margin for 3 consecutive jobs

# Queue limits for bridge offers (may differ from local GPU limits)
max_queue_depth: 10               # bridge jobs queue less deep than local GPU (latency + cost)
```

### B. compute_pricing (Bridge Variant)

Bridge offers use the same `compute_pricing` contribution schema as local GPU offers. The difference is floor derivation:

- **Local GPU:** `floor_per_m_input` and `floor_per_m_output` are set manually by the operator (covers electricity, hardware amortization).
- **Bridge:** `floor_per_m_input` and `floor_per_m_output` are COMPUTED by the bridge daemon from OpenRouter pricing × credit/dollar rate. The operator can override upward but not below the computed floor.

```yaml
# compute_pricing for a bridge offer
model: meta-llama/llama-3.1-70b-instruct
provider_type: bridge                    # distinguishes from local GPU pricing for same model
pricing_mode: competitive
competitive_target: match_best
competitive_offset_bps: 0
# Auto-derived floors (bridge daemon computes and writes these):
floor_per_m_input: 3000                  # ceil($0.30/M * 10000 credits/$)
floor_per_m_output: 4000                 # ceil($0.40/M * 10000 credits/$)
# Operator-set ceilings:
ceiling_per_m_input: 20000
ceiling_per_m_output: 30000
# Fixed rate fallback:
rate_per_m_input: 5000
rate_per_m_output: 8000
reservation_fee: 2
queue_discount_curve:
  - depth: 0
    multiplier_bps: 10000
  - depth: 3
    multiplier_bps: 9000
  - depth: 5
    multiplier_bps: 7500
max_queue_depth: 10
```

The bridge daemon is the sole writer of `floor_per_m_input` and `floor_per_m_output` on bridge variant contributions. The generative config UI shows these as read-only with a "(auto-derived from OpenRouter pricing)" label.

---

## VII. Frontend Workstream

### A. BridgeConfigPanel.tsx

New panel in the Market tab (alongside existing QueueLiveView). Accessible when the operator has an OpenRouter key configured.

**Controls:**

1. **Enable Bridge toggle** — Master switch. Off by default. Enabling triggers model scan.
2. **Bridge API Key** — Input field for the bridge-dedicated OpenRouter key. Separate from the primary key shown in Settings. Validated on save (test call to `/api/v1/models`).
3. **Model Allowlist** — Checkbox list populated from OpenRouter `/api/v1/models` response. Shows model name, OpenRouter pricing, computed credit floor. Operator checks models to offer.
4. **Margin Strategy** — Radio: "Fixed margin" (with BPS input) or "Competitive" (with target dropdown and offset BPS input).
5. **Safety Controls:**
   - Monthly dollar limit input
   - Pause on negative margin toggle
   - Max queue depth for bridge offers
6. **Status indicators:**
   - Per-model: active / suspended (with reason) / inactive
   - Global: bridge active / paused / suspended (with reason)
   - Last model refresh timestamp
   - Current dollar spend this period vs limit

**IPC commands:**

- `bridge_config_get` → returns current `compute_bridge` contribution
- `bridge_config_save` → validates and supersedes `compute_bridge` contribution
- `bridge_status_get` → returns live bridge state (per-model status, spend, last refresh)
- `bridge_model_scan` → triggers immediate model availability scan

### B. Bridge Economics View

Sub-view of the Market tab showing dual-currency economics:

| Column | Source |
|---|---|
| Job ID | Wire job ID (semantic path, not UUID) |
| Model | OpenRouter model ID |
| Credit Revenue | credits earned from Wire settlement |
| Dollar Cost | USD paid to OpenRouter |
| Margin (credits) | credit revenue - (dollar cost × credit/dollar rate) |
| Margin (%) | margin / credit revenue × 100 |
| Timestamp | completed_at |

**Aggregates at top:**
- Total credit revenue (session / all-time)
- Total dollar cost (session / all-time)
- Effective margin (session / all-time)
- Net P&L in credits (session / all-time)

Data source: chronicle events (`bridge_cost_recorded`) joined with the bridge daemon's local dollar cost ledger.

### C. Privacy Disclosure on Market Surface

When browsing the market surface (Phase 2-3 frontend):

- Bridge provider offers show a "Cloud Relay" badge next to the model name
- Hovering the badge shows: "Inference relayed through OpenRouter. Your prompt leaves the Wire network and is processed by external infrastructure."
- Filter control: "Include cloud relay providers" checkbox (default: checked, governed by requester's `dispatch_policy.bridge_policy.allowed`)
- Bridge offers sort AFTER local GPU offers at the same price (preference for on-network inference)

---

## VIII. Verification Criteria

All pass/fail. No partial credit.

1. **Happy path:** Node A does a pyramid build. Node B serves as bridge (has no local GPU, only bridge mode). B receives the job, calls OpenRouter, returns result. Credits flow from A to B. Build produces valid pyramid output.

2. **Economics visibility:** After the build, Node B's Bridge Economics View shows: credit revenue for the job, dollar cost from OpenRouter, positive margin. Node B's chronicle has `bridge_dispatched`, `bridge_returned`, `bridge_cost_recorded` events.

3. **Error handling (429):** Simulate OpenRouter 429 (rate limited). Verify: bridge retries up to 3 times with exponential backoff. If all retries fail, Wire job transitions to `failed` with `failure_reason = 'upstream_rate_limited'`. Bridge offers remain active.

4. **Error handling (503):** Simulate OpenRouter 503 for a specific model. Verify: Wire job fails with `failure_reason = 'upstream_model_unavailable'`. That model's bridge offer transitions to `status='suspended'`. Other models' offers remain active.

5. **Error handling (402):** Simulate OpenRouter 402 (insufficient funds). Verify: Wire job fails with `failure_reason = 'upstream_funds_exhausted'`. ALL bridge offers for this node transition to `status='suspended'`. Bridge mode indicator shows "Suspended: OpenRouter funds exhausted."

6. **Model lifecycle:** Remove a model from the operator's allowlist in BridgeConfigPanel. Verify: that model's bridge offer deactivates within one refresh interval. Add it back: offer reactivates.

7. **Model lifecycle (external):** Mock OpenRouter `/api/v1/models` to stop returning a previously available model. Verify: bridge offer for that model transitions to `status='suspended'` with reason `'upstream_model_unavailable'` within one refresh interval.

8. **Rate isolation:** Initiate a personal build and a bridge job simultaneously. Verify: they use different API keys (inspect request headers in the cost log). Rate limiting on the bridge key does not block the personal build.

9. **Privacy disclosure:** Browse the market surface as a requester. Verify: bridge offers show "Cloud Relay" badge. Filter "exclude cloud relay" — bridge offers disappear from results.

10. **No-bridge dispatch policy:** Set `bridge_policy.allowed = false` in requester's dispatch_policy contribution. Verify: matching RPC never returns bridge offers for this requester, even if bridge offers are the cheapest available.

11. **Floor auto-adjustment:** Change the credit/dollar rate economic_parameter. Verify: bridge offer floors recalculate. If current rate drops below new floor, offer rate auto-adjusts upward.

12. **Fleet ordering:** Operator has both local GPU (Node B) and bridge-only (Node C) fleet peers. Verify: fleet dispatch prefers Node B (local GPU, free) over Node C (bridge, costs dollars). Node C is only used when Node B is unavailable or at capacity.

13. **DADBEAR integration:** Bridge job creates a DADBEAR work item with source "bridge" and semantic path "bridge/{model_id}/{job_id}". Preview gate evaluates dollar cost vs credit revenue. Job with negative estimated margin gets `cost_limit` hold.

---

## IX. Handoff to Phase 5

Phase 4 delivers:
- Bridge provider type fully operational with dual-currency settlement
- Error classification and automatic offer suspension/recovery
- Model lifecycle management with periodic OpenRouter sync
- Privacy disclosure on market surface
- Rate-isolated API keys for bridge vs personal use
- Fleet dispatch ordering that prevents same-operator dollar waste
- DADBEAR work item integration for bridge jobs
- Complete chronicle trail for bridge operations
- Bridge economics view for operator P&L visibility

Phase 5 (Quality & Challenges) builds on this with:
- Quality enforcement extending to bridge-specific concerns: does the bridge actually relay to the model it claims? Are bridge response times consistent with honest relay (not cached stale responses)?
- Challenge protocol for bridge jobs: evidence includes OpenRouter trace data (via broadcast webhook correlation) and timing analysis
- Bridge-specific honeypot testing: Wire dispatches known-answer jobs to bridge providers and verifies correct model is used (model fingerprinting via response characteristics)

---

## X. Audit Corrections Applied

| Audit Finding | Section | How Addressed |
|---|---|---|
| **6a: No dollar-to-credit conversion** [Critical] | II | Full mechanical specification: read OpenRouter pricing API, compute floor via `ceil(price × rate)`, auto-adjust on price/rate changes |
| **6b: Double-settlement failure modes** [Critical] | III (Double-Settlement Failure Handling) | Settlement ordering specified: Wire deposit locks BEFORE OpenRouter dispatch. Unrecoverable loss case documented with chronicle event. |
| **6c: Rate limit isolation** [Critical] | V.B | Bridge-dedicated API key required. Two provisioning paths (programmatic via Management API, manual fallback). Separate from personal key in all code paths. |
| **6d: Error code mapping** [Critical] | III | Exhaustive table: 12 OpenRouter HTTP codes mapped to Wire job states, offer states, retry policy, and failure_reason enum. |
| **6e: Model lifecycle** [Critical] | V.C | Periodic refresh from OpenRouter models API. Diff-and-deactivate for removed models. Diff-and-activate for new models. Floor recalculation on price changes. |
| **6f: Fleet vs bridge dispatch ordering** [Major] | V.D | `provider_types` field on FleetPeer. Two-pass fleet dispatch: local GPU first, bridge only when local exhausted. Same-operator bridge is last resort. |
| **6g: Cloudflare 120s timeout** [Major] | V.A (step 3 notes) | Bridge dispatch uses the same ACK+async pattern that Phase 3 implements for all market jobs. The bridge is a consumer of this pattern, not a new implementation. |
| **6h: compute_bridge contribution undefined** [Major] | VI.A | Full YAML schema with all fields, defaults, and operational semantics. |
| **3d: Bridge privacy degradation** [Critical] | IV.B | Bridge offers carry `privacy_capabilities: '{cloud_relay}'`. Market surface shows Cloud Relay badge. Dispatch policy allows requester-side filtering via `bridge_policy.allowed`. |
| **Theme 1: DADBEAR integration** [Major for Phase 4] | V.F | Bridge jobs as DADBEAR work items. Source "bridge", semantic paths, preview gate with margin check, hold on negative margin. |
| **Theme 2: Chronicle events** [Major for Phase 4] | V.G | Seven chronicle event types defined with all fields. All carry work_item_id and attempt_id for DADBEAR correlation. |
