# Phase 2: Exchange & Matching

**What ships:** Providers publish offers. Requesters match jobs. Wire mirror active. Jobs flow through the exchange. Market jobs enter the compute queue on the provider side via DADBEAR work items.

**Prerequisites:**
- Phase 1 compute market foundation (shipped)
- DADBEAR canonical architecture (shipped)
- Async-fleet-dispatch Phases 1-3 (shipped): `TunnelUrl`, `FleetIdentity` verifier pattern, `fleet_result_outbox` + CAS helpers, `fleet_delivery_policy` contribution pattern
- Fleet MPS WS1 — `compute_participation_policy` extended to 10 canonical fields per DD-I (NOT yet shipped — current struct at `local_mode.rs:1719-1727` has 5 fields; Phase 2 either waits for Fleet MPS extension or absorbs the struct extension into its own node-side workstream)
- Fleet MPS WS2 — three runtime objects (`ServiceDescriptor` + `AvailabilitySnapshot` + `PeerKnowledgeState`) designed in `fleet-mps-three-objects.md`, NOT yet shipped. Phase 2 consumes these for offer content derivation and admission. Phase 2 either waits for WS2 or absorbs the three-objects build into its own node-side workstream.

**Architecture doc:** `GoodNewsEveryone/docs/architecture/wire-compute-market.md` (canonical schemas, credit flow, privacy model)
**Build plan:** `agent-wire-node/docs/plans/wire-compute-market-build-plan.md` (full RPC SQL, table CREATE statements)

---

## I. Overview

Phase 2 turns the Phase 1 local-only compute queue into a market-connected exchange. After this phase:

- A provider node can publish standing offers (model, rates, discount curve) to the Wire.
- An external requester (via curl or future Phase 3 requester integration) can match a job against those offers, creating a reserved slot on the provider's queue.
- The fill RPC accepts input token counts + a ChatML `messages` payload that the Wire forwards to the provider as part of the dispatch envelope. The Wire acts as a transient bootstrap relay node (not a permanent privacy-stripping proxy) — the protocol shape matches the mature network with Wire-as-one-relay-among-many.
- The Wire dispatches filled jobs to the provider's tunnel URL.
- The provider receives the job, ACKs 202, creates a DADBEAR work item, enqueues to the compute queue, GPU processes; result flows back via the Phase 3 outbox delivery worker (owned by Phase 3).
- The queue mirror pushes state changes to the Wire on every mutation.
- The market surface endpoint exposes pricing, queue depths, and provider availability.

**What Phase 2 does NOT include:**
- Requester-side integration (WireComputeProvider, chain executor dispatch) — Phase 3.
- Settlement + outbox delivery worker (settle/fail/void RPCs exist from Phase 1 migration but aren't called by node code yet) — Phase 3.
- Bridge operations — Phase 4.
- Relay chain (`relay_count > 0` rejected inline in `fill_compute_job`; no separate `select_relay_chain` function) — Relay market plan.

**Privacy model for this phase:** per architecture §III (canonical SOTA model) and DD-A/DD-B/DD-D in §VIII.6. Three orthogonal mechanisms apply from launch: variable relay count, distributional opacity, tunnel rotation. At launch, before relay capacity exists on the network, the Wire acts as a **transient bootstrap relay node** — the `CallbackKind::MarketStandard` variant's `callback_url` points at the Wire's `/v1/compute/result-relay` endpoint, and prompts flow through the Wire to the provider. This is the **bootstrap mode** described in architecture §III, not a permanent "Wire-proxied standard tier." As non-Wire relay nodes deploy, the `callback_url` value shifts to those relays (for `relay_count > 0`) or to the requester's tunnel directly (for 0-relay post-bootstrap) — the protocol shape is identical. The Wire never persists prompt or result content — the bootstrap relay endpoint is forward-then-forget.

---

## II. Wire Workstream

### Migrations

Tables this phase reads/writes:
- `wire_compute_offers` -- provider standing offers (Phase 1)
- `wire_compute_jobs` -- job lifecycle records (Phase 1)
- `wire_compute_queue_state` -- per-(node, model) queue mirror (Phase 1)
- `wire_compute_observations` -- performance observations (Phase 1)
- `wire_market_rotator` -- rotator arm state (Phase 1)
- `fleet_result_outbox` -- reused for market dispatches per DD-D. **Per DD-Q: an ALTER migration adds the `callback_kind` column to the shipped table (currently has 13 columns, no callback_kind). The sweep helpers must be updated to read the new column during orphan-promotion. `validate_callback_url` at `fleet.rs:659-661` must be extended to accept-any-HTTPS for MarketStandard/Relay (currently returns `KindNotImplemented`).** These are mechanical prereqs, not a new table.
- `pyramid_market_delivery_policy` -- NEW singleton (per DD-Q + DD-E). DDL added to `db::init_pyramid_db`, parallel to `pyramid_fleet_delivery_policy`.

**Phase 2 Workstream 0 — DD-Q pre-flight migrations (apply before any handler work):**

0.1. Node-side DB migration: `ALTER TABLE fleet_result_outbox ADD COLUMN callback_kind TEXT NOT NULL DEFAULT 'Fleet'`. Plus `CREATE INDEX idx_fleet_outbox_callback_kind`.
0.2. Node-side DB migration: `CREATE TABLE IF NOT EXISTS pyramid_market_delivery_policy (id INTEGER PRIMARY KEY CHECK (id = 1), yaml TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT (datetime('now')))`.
0.3. Code change `fleet.rs:659-661`: extend `validate_callback_url` for `MarketStandard | Relay` — require `scheme == "https"`, host non-empty, otherwise return new `CallbackValidationError::SchemeNotHttps` / `MissingHost` variants. Localhost allowed.
0.4. Code change `db.rs` sweep helpers: SELECT the new `callback_kind` column when reconstructing `CallbackKind` for revalidation on `pending → ready` orphan promotion. Dispatcher sentinel `"wire-platform"` for market rows skips the roster lookup (the callback URL is JWT-gated).
0.5. Canonical sentinel: every market dispatch's outbox row uses `dispatcher_node_id = "wire-platform"`. Document in `db.rs` alongside the `fleet_result_outbox` DDL so future readers see it.

**Phase 2 Wire-side migrations required (after Workstream 0):**

1. **Extend `fill_compute_job` signature** (per DD-J):
   - Add `p_relay_count INTEGER DEFAULT 0` — reject with `RAISE EXCEPTION 'Relay chain not available — relay_count must be 0 at launch'` if > 0.
   - Add `p_requester_operator_id UUID` — needed for the self-dealing guard check and for later clawback provenance.
   - Extend return TABLE to include `provider_node_id UUID` — needed by the API route to populate the `pid` claim in the issued `wire_job_token`.
   - Fix: `v_wire_platform_operator_id` declaration (audit Theme 4 SQL bug) — add the same resolution query used in `match_compute_job` at the top of the function body.
   - Do NOT return `provider_tunnel_url` to the caller (audit S3). Wire uses it internally for dispatch but never exposes it via the API.

2. **Extend `match_compute_job`**:
   - Self-dealing guard (audit Theme 5g): inline WHERE clause filter `AND o.operator_id != p_requester_operator_id` in the offer SELECT.
   - Replace the hardcoded `COALESCE(..., 500)` output estimate fallback (audit S10/item 10) with a read of the `default_output_estimate` economic_parameter contribution. Fall back to 500 only if no contribution exists.

3. **Add `start_compute_job(p_job_id UUID, p_provider_node_id UUID) RETURNS void`** (canonical per DD-J; moved out of architecture §IX):
   - CAS transitions the job row from `'filled' → 'executing'`, sets `dispatched_at = now()`.
   - Called by the provider's GPU loop immediately before dequeuing a market job and entering the LLM call.

4. **Fix queue-decrement `model_id` filter** on `settle_compute_job`, `fail_compute_job`, `void_compute_job` (audit Theme 4): add `AND model_id = v_job.model_id` to each RPC's `UPDATE wire_compute_queue_state` clause. Remove the duplicate Wire-platform-operator resolution inside `settle_compute_job`'s `v_requester_adj < 0` branch — the top-of-function resolution is sufficient.

5. **Seed `market_delivery_policy` contribution** — **node-side bundled** (per DD-E; follows fleet-mps WS1 pattern: seed YAML ships at `docs/seeds/market_delivery_policy.yaml`, the bundled JSON at `src-tauri/assets/bundled_contributions.json` gets a schema_definition entry + a default contribution row for `compute_participation_policy`'s sibling). Loader writes to the `pyramid_market_delivery_policy` singleton table. 17-field shape-parallel to fleet_delivery_policy (per corrected DD-E):
   ```yaml
   schema_type: market_delivery_policy
   version: 1
   # Dispatcher side (for provider when acting as callback sender)
   callback_post_timeout_secs: 30
   outbox_sweep_interval_secs: 15
   worker_heartbeat_interval_secs: 10
   worker_heartbeat_tolerance_secs: 30
   backoff_base_secs: 1
   backoff_cap_secs: 64
   max_delivery_attempts: 20
   ready_retention_secs: 1800
   delivered_retention_secs: 3600
   failed_retention_secs: 604800
   # Admission control
   max_inflight_jobs: 32
   admission_retry_after_secs: 30
   # Market-specific (absorbed from individual economic_parameter contributions per DD-E)
   match_search_fee: 1
   offer_creation_fee: 1
   queue_push_fee: 1
   queue_mirror_debounce_ms: 500
   ```
   Seed YAML ships at `docs/seeds/market_delivery_policy.yaml`. Loaded into `pyramid_market_delivery_policy` singleton table (new — parallel to `pyramid_fleet_delivery_policy`). Hot-reload via `config_contributions::sync_config_to_operational_with_registry` (new match arm). Rust struct + `Default` impl at `pyramid/market_delivery_policy.rs` holds bootstrap sentinels only.

6. **Seed `fill_job_ttl_secs` economic_parameter** — **Wire-side** (per DD-G; consumed by the Wire's `/api/v1/compute/fill` route handler when signing the wire_job_token). Seeded via supabase migration as a `wire_contributions` row with `type = 'economic_parameter'`, `structured_data -> parameter_name = 'fill_job_ttl_secs'`, `ttl_secs: 300`.

7. **Seed `max_completion_token_ratio` economic_parameter** — **Wire-side** (per DD-M; consumed by Phase 3's `settle_compute_job` RPC). Seeded via supabase migration as a `wire_contributions` row with `parameter_name = 'max_completion_token_ratio'`, `ratio: 2`. Phase 3's settle rewrite reads it and removes the hardcoded `* 2` guard.

**Seed location convention (per DD-E/DD-G/DD-M):**
- **Node-side bundled contributions** (follow fleet-mps WS1 pattern): `market_delivery_policy`, `compute_participation_policy` (extended), `privacy_policy`. These ship in `src-tauri/assets/bundled_contributions.json` with a schema_definition + default contribution row; loader seeds the node's local `pyramid_wire_contributions` on first boot.
- **Wire-side economic_parameter seeds**: `fill_job_ttl_secs`, `max_completion_token_ratio`, `market_rotator_config` (already shipped), `staleness_thresholds` (already shipped), `compute_deposit_config` (already shipped), `default_output_estimate` (already shipped). These ship in Wire supabase migrations as `wire_contributions` rows with `type = 'economic_parameter'`. Consumed by Wire RPCs at match/fill/settle time.

The split is: node-reads → node-bundled; Wire-reads → Wire-seeded. A contribution read by both (none currently) would live on the Wire and sync to the node via the contribution-sync mechanism. No cross-side seeding of the same value.

### RPC canonical locations (per DD-J)

| RPC | Canonical location | Phase |
|---|---|---|
| `match_compute_job` | Deployed (Phase 1) + this phase's audit patches | Phase 2 patches |
| `fill_compute_job` | Deployed (Phase 1) + this phase's signature extension | Phase 2 extension |
| `start_compute_job` | **This phase — new.** | Phase 2 |
| `compute_queue_multiplier_bps` | Deployed (Phase 1). No changes. | — |
| `deactivate_stale_compute_offers` | Deployed. Reads `staleness_thresholds.heartbeat_staleness_s` (seconds). Phase 5 extends to preserve hold statuses. | — (Phase 5 patches) |
| `settle_compute_job` / `fail_compute_job` / `void_compute_job` | Deployed + queue-decrement patch in this phase (fix #4 above). | Phase 2 patches |
| `cancel_compute_job` | Phase 3 §II — new. | Phase 3 |
| `sweep_timed_out_compute_jobs` | Deployed. No changes. | — |

Architecture §IX is the canonical reference of the deployed state + patches applied here; no parallel SQL bodies in other docs.

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
- Action: Fill a reserved slot with input token count, charge deposit, dispatch to provider
- Body: `{ job_id, input_token_count, temperature, max_tokens, messages, relay_count }`  *(messages: ChatML per DD-C; relay_count rejected if > 0 at Phase 2 launch)*
- RPC: `fill_compute_job` — per DD-J, Phase 2 migration extends the deployed signature to add `p_relay_count INTEGER DEFAULT 0` (reject >0), `p_requester_operator_id UUID`, and returns `provider_node_id UUID` in the TABLE result (needed for the `pid` claim in `wire_job_token`).
- Response: `{ deposit_charged, relay_chain, provider_ephemeral_pubkey, total_relay_fee }` *(relay_chain is `[]` and provider_ephemeral_pubkey is null until relay market ships)*
- **`wire_job_token` issuance (per DD-G):** After `fill_compute_job` returns successfully, the route handler constructs a `MarketClaims` JWT:
  ```
  aud = "compute"
  iss = wire signing key identifier (same as fleet)
  exp = now + fill_job_ttl_secs  (from economic_parameter; default 300 = 5min)
  iat = now
  sub = job_id                   (from RPC result)
  pid = provider_node_id         (from RPC result)
  ```
  Signs with the Wire's private key (shared signing key used for fleet JWT, dashboard query tokens, etc.). Embeds in the `Authorization: Bearer` header on the outbound `POST {provider_tunnel}/v1/compute/job-dispatch` request. The body of that request is the `MarketDispatchRequest` (see Node Workstream §III).
- **`fill_job_ttl_secs` seed:** Phase 2 migration seeds a new `economic_parameter` contribution:
  ```yaml
  parameter_name: fill_job_ttl_secs
  ttl_secs: 300
  ```
  5 minutes is generous — typical dispatch ACK completes in seconds; TTL is a safety net for tunnel hiccups.
- After fill succeeds + token issued: the route handler dispatches to the provider's tunnel URL. This is Wire-internal — the requester never sees the tunnel URL.

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

### Integration with Fleet MPS (compute_participation_policy / ServiceDescriptor / AvailabilitySnapshot)

Phase 2 integrates with Fleet MPS for operator-intent gating and for offer content derivation. **Prerequisites (not yet shipped — must land before Phase 2 node-side work):**
- Fleet MPS WS1 (compute_participation_policy) extended to the 10 canonical fields per `fleet-mps-build-plan.md` + DD-I (architecture §VIII.6). Current shipped struct at `local_mode.rs:1719-1727` has only 5 fields — the missing `allow_market_dispatch` + 4 storage/relay fields must land as part of the Fleet MPS extension.
- Fleet MPS WS2 (ServiceDescriptor + AvailabilitySnapshot + PeerKnowledgeState three-objects) shipped in code. These are DESIGNED in `fleet-mps-three-objects.md` but not yet built — currently parallel state exists on `FleetPeer` and various runtime checks. Phase 2 needs the structs.

**If the Fleet MPS extension hasn't shipped when Phase 2 starts, the node-side workstream must include: (a) extending `ComputeParticipationPolicy` to 10 fields + the projection function, (b) landing the three MPS runtime objects. These are logically separate workstreams but Phase 2 cannot proceed without them.**

**`compute_participation_policy` gating (per DD-I — canonical 10 fields):**

Phase 2 consumes `allow_market_visibility` at two points:
1. **Offer publication** — `POST /api/v1/compute/offers` is gated. If `allow_market_visibility == false`, no offer publish calls fire. Existing offers get deactivated if this flips false.
2. **Market job acceptance** — the `/v1/compute/job-dispatch` handler rejects with 503 (with an explanatory `X-Wire-Reason: market_serving_disabled` header) if `allow_market_visibility == false`.

Phase 3 consumes `allow_market_dispatch`:
- The dispatch chain only considers `wire-compute` as a provider if `allow_market_dispatch == true`. A worker-mode node cannot dispatch outward even if its dispatch policy lists `wire-compute`.

The full 10-field canonical list and the mode-to-booleans projection function are defined in `fleet-mps-build-plan.md` "Durable Contribution" section + DD-I in architecture §VIII.6. No re-declaration here.

**`ServiceDescriptor` drives offer content:**

Offers published by Phase 2 are NOT hand-constructed. They derive from `ServiceDescriptor`:
- `models_loaded` → one offer per loaded model (filtered by `servable_rules`)
- `servable_rules` → included as `rule_name` metadata on the offer (so market requesters can route by rule_name the same way fleet does)
- `visibility` field on the descriptor: if `market-visible` the offer publishes; if `private fleet` or `disabled` no market offer
- `protocol_version` → included as a compatibility gate on the offer (requesters matching against an incompatible protocol_version skip it)

When `ServiceDescriptor` changes (model loaded/unloaded, rule set edited), the offer update is emitted — one pass through the single reducer, one resulting Wire API call. No parallel "sync offers" logic.

**`AvailabilitySnapshot` drives acceptance:**

The job-dispatch handler's admission checks (step 2 in the handler flow) read from `AvailabilitySnapshot`:
- `total_queue_depth` vs `max_market_depth` — existing gate
- `health_status`: if `degraded` AND `allow_serving_while_degraded == false`, reject with 503
- `tunnel_status`: if not `healthy`, reject (can't deliver results back)

The availability version is included in the 503 response so the Wire's offer-staleness cleanup can correlate ("this node's availability is behind, deactivate its offers").

**What this replaces:** The Phase 1 placeholder `pub enabled: bool` on `ComputeMarketState` is REMOVED. It was a pre-MPS proxy for "market participation." Replaced entirely by `compute_participation_policy.allow_market_visibility`. The `is_serving` field on `ComputeMarketState` stays — it reflects whether the bootstrap initialization completed and the mirror loop is running (runtime state, not operator intent).

### compute_market.rs: Full Market State

Replace the Phase 1 stub (`pub struct ComputeMarketState { pub enabled: bool }`) with full market state.

```rust
/// Persisted to `${app_data_dir}/compute_market_state.json` (same location pattern as
/// existing `dadbear_state.json`, `fleet_roster.json`, etc.). Struct carries a
/// `schema_version: u32` field; load() returns None + logs a warn on version mismatch
/// (cold-start rebuild, no in-place migration for Phase 2 — add migration step when
/// Phase 2 fields change in a later phase). Phase 1's `pub enabled: bool` stub is
/// removed from the Rust struct; the on-disk JSON silently drops it on next save
/// (serde ignore_unknown_fields applies to load; save writes only current fields).
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

**Batching/debounce:** Use a debounce window read from `market_delivery_policy.queue_mirror_debounce_ms` (per DD-E, absorbed into the unified policy contribution — NOT a standalone economic_parameter). Default bootstrap sentinel: 500ms. Multiple queue mutations within the window coalesce into a single push. This matters because DD-9 economic-gate charges 1 credit per push (cost absorbed via `market_delivery_policy.queue_push_fee`). Reader path: the mirror-push task holds `Arc<RwLock<MarketDeliveryPolicy>>` (in `MarketDispatchContext`) and reads the debounce value on each tick — hot-reload applies automatically when the contribution is superseded.

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

**Push failure backoff:** On any push failure (network error, 5xx), use exponential backoff. Record a chronicle event `queue_mirror_push_failed` with the error. **P4 Pillar 37 — no hardcoded constants.** Backoff is read from `market_delivery_policy.backoff_base_secs` and `market_delivery_policy.backoff_cap_secs` (DD-E absorbs the queue-mirror push backoff into the unified market policy; no separate `queue_mirror_backoff_schedule` contribution). Cold-start bootstrap sentinel if policy not yet loaded: `backoff_base_secs=1, backoff_cap_secs=30` (anti-bootstrap-deadlock only, not operational spec).

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

**This endpoint follows the same ACK+callback+outbox protocol shape as `handle_fleet_dispatch`** (`async-fleet-dispatch.md`). The compute market is literally the fleet protocol with a different JWT audience (`compute` vs `fleet`) and a different callback destination. All the systemic scaffolding defined for fleet dispatch applies here without modification:

| Fleet primitive | Reused for market |
|---|---|
| `TunnelUrl` newtype | Same — all URL fields on the market envelope are `TunnelUrl` (parsed at ingress, normalized once) |
| `FleetIdentity` verifier pattern | Parallel `MarketIdentity` verifier: same shape (`verify_market_identity(bearer, public_key, ...) -> Result<MarketIdentity, MarketAuthError>`), different `aud` claim (`"compute"` not `"fleet"`). Single source of truth for market JWT verification. |
| `FleetDispatchAck { job_id, peer_queue_depth }` | Same shape returned as the 202 ACK body (renamed `MarketDispatchAck`; identical fields) |
| `FleetAsyncResult` tagged enum | Same shape for the deferred result payload (`Success \| Error`); renamed `MarketAsyncResult`, identical variants |
| Outbox table pattern (`expires_at` + `worker_heartbeat_at`, CAS updates, compound PK) | **Per DD-D: reuse the shipped `fleet_result_outbox` table, no parallel `compute_result_outbox`.** Per DD-Q, an ALTER migration adds `callback_kind TEXT NOT NULL DEFAULT 'Fleet'`. Compound PK `(dispatcher_node_id, job_id)`: for market dispatches, `dispatcher_node_id = "wire-platform"` (sentinel — the Wire is not a peer and has no node_id in the fleet-roster sense; job_id uniqueness + callback_kind discriminator make collisions structurally impossible). Sweep helpers read the callback_kind column to synthesize the right `CallbackKind` for `validate_callback_url` revalidation. |
| `FleetDispatchContext` Arc bundle | Parallel `MarketDispatchContext` Arc bundle with same discipline: `tunnel_state` (borrowed from AppState), `pending: Arc<PendingMarketJobs>` (owned), `policy: Arc<RwLock<MarketDeliveryPolicy>>` (owned). |
| Startup recovery (`pending → ready` with synth Error) | Same pattern for market jobs held in the outbox across restarts — the existing sweep loop handles them by discriminating on `callback_kind`. |
| `validate_callback_url` | Extended for non-Fleet variants: `Fleet` checks roster (existing); `MarketStandard` and `Relay` accept any HTTPS URL because the callback is JWT-gated (the `wire_job_token` signature proves the URL came from the Wire). |

**Request body (per DD-C — messages shape diverges from fleet with a documented helper):**
```rust
pub struct MarketDispatchRequest {
    // Matches FleetDispatchRequest field-for-field EXCEPT the prompt shape.
    // Fleet uses (system_prompt: String, user_prompt: String); market uses messages.
    // Provider-side converts at the handler boundary via messages_to_prompt_pair (see below).
    pub job_id: String,                       // UUID generated by Wire at match time
    pub model: String,
    pub messages: serde_json::Value,          // ChatML array of {role, content}
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub response_format: Option<serde_json::Value>,
    /// Where to deliver the result. Under SOTA privacy model (see architecture §III):
    ///   - Launch (bootstrap mode): Wire's relay endpoint, `{wire_base}/v1/compute/result-relay`
    ///   - Post-relay-market, 0-relay: requester's tunnel URL
    ///   - Post-relay-market, N-relay: first relay hop's tunnel URL
    /// The provider treats this as opaque — it just POSTs the result here.
    pub callback_url: TunnelUrl,

    pub credit_rate_in_per_m: i64,
    pub credit_rate_out_per_m: i64,
    pub privacy_tier: String,                 // "standard" | "cloud_relay" | future
}
```

**`messages_to_prompt_pair` helper (per DD-C).** Lives at `pyramid/messages.rs` (new module, ~30 lines). Converts `messages: Value` to `(system_prompt: String, user_prompt: String)` for the downstream `QueueEntry` / Ollama call path.

```rust
pub enum MessagesError {
    InvalidShape,        // not a JSON array, or elements are not objects
    UnknownRole(String), // role field is not "system" | "user" | "assistant"
    NoUserMessages,      // no user messages present
    AssistantTurns,      // conversation includes assistant turns (reject in Phase 2)
}

pub fn messages_to_prompt_pair(
    messages: &serde_json::Value,
) -> Result<(String, String), MessagesError> {
    // 1. Parse as Vec<{role, content}>. Reject on InvalidShape if the array is malformed.
    // 2. Collapse: first "system" message → system_prompt (empty string if none).
    // 3. Concatenate all "user" messages with "\n\n" → user_prompt.
    // 4. Reject "assistant" messages with AssistantTurns (Phase 2 is single-turn only).
    // 5. Reject NoUserMessages if no user messages present.
}
```

The provider-side job-dispatch handler calls this between idempotent-outbox-insert (step 3) and DADBEAR-work-item-creation (step 4). On error: return 400 with the specific `MessagesError` in the response body. Phase 4 (bridge) reuses this helper — bridge's OpenRouter request already expects messages format, but the helper still validates structure before the network call.

**Auth — header only, not body.** The `wire_job_token` JWT goes in the `Authorization: Bearer <token>` header. Not in the body. `verify_market_identity` is the handler's first action. No body-level JWT, no `#[serde(alias = ...)]` on claim names — same discipline as `FleetIdentity`.

**MarketIdentity verifier (per DD-F in architecture §VIII.6).** Lives at `pyramid/market_identity.rs`, parallel to `fleet_identity.rs`. Verifies the `wire_job_token` JWT:

```rust
pub struct MarketClaims {
    pub aud: String,       // must be "compute"
    pub iss: String,       // Wire's key identifier (same as fleet)
    pub exp: i64,
    pub iat: i64,
    pub sub: String,       // job_id (UUID string)
    pub pid: String,       // provider node_id — provider checks == self.node_id
}

pub fn verify_market_identity(
    bearer: &str,
    public_key: &str,       // same key as fleet: AuthState.jwt_public_key
    self_node_id: &str,
) -> Result<MarketIdentity, MarketAuthError> {
    // 1. jsonwebtoken::decode with set_audience(&["compute"]) and validate_exp=true
    // 2. Require claims.pid == self_node_id (provider-binding; fleet's op-check equivalent)
    // 3. Require claims.sub non-empty (job_id bound)
    // 4. Return MarketIdentity { pid, sub_job_id }
}
```

Public key source: `AuthState.jwt_public_key` (same key the fleet verifier uses — Wire has one signing key; the `aud` claim differentiates). Error variants: `InvalidToken / ProviderMismatch / MissingJobId / MissingSelfNodeId`. Unit tests assert: non-compute aud rejected, missing/empty pid rejected, expired token rejected, valid returns populated MarketIdentity.

**Handler flow (ACK path, returns within seconds):**

1. **Verify `wire_job_token` JWT** via `verify_market_identity(bearer, public_key, self_node_id)`. Reject on any failure with 401/403.

2. **Check admission** — per DD-H in architecture §VIII.6. Reject with 503 (with `Retry-After` header from market delivery policy) if any of the following active DADBEAR holds on the `"market:compute"` slug apply:
   - Blocking holds: `frozen`, `breaker`, `cost_limit`, `quality_hold`, `timing_suspended`, `reputation_suspended`, `suspended`, `escalation`
   - Marker holds (`measurement`, etc.) are NOT blocking — informational only
   
   Also reject with 503 if queue capacity exhausted (`queue_depth >= max_market_depth`), or the Phase 5 negative-balance gate trips.

3. **Idempotent outbox insert** — per DD-D, reuse the existing `fleet_result_outbox` table (not a parallel `compute_result_outbox`). Compound PK `(dispatcher_node_id, job_id)`: per DD-Q, `dispatcher_node_id = "wire-platform"` (sentinel string constant — the Wire is not a fleet peer). The `callback_kind` column (added by DD-Q ALTER migration) is `'MarketStandard'` or `'Relay'` per the envelope's CallbackKind. `INSERT ... ON CONFLICT DO NOTHING` — if retry of same dispatch, worker_heartbeat_tolerance keeps the row alive; return 202 with existing `job_id`. No double-GPU.

4. **Create DADBEAR work item** (state `"previewed"` — see Section V P3 fix for why provider-side preview is a no-op).

5. **Enqueue** to `compute_queue` with `source: "market_received"`, `work_item_id`, `attempt_id`, and a reference to the outbox row `(dispatcher_node_id, job_id)` tuple (so the GPU loop can CAS the result into the right row).

6. **Record chronicle event** `market_received`.

7. **Trigger queue mirror push** (debounced per policy).

8. **Return HTTP 202** with `MarketDispatchAck { job_id, peer_queue_depth }`. Do NOT hold the HTTP connection open past this point.

**Worker path (GPU loop, any duration):**

- Worker tick bumps `expires_at = now + worker_heartbeat_tolerance_secs` every `worker_heartbeat_interval_secs` (both from market delivery policy).
- On LLM completion: CAS update `status='ready'`, write result_json, bump `expires_at = now + ready_retention_secs`.
- Phase 3 dispatch loop delivers the result to `callback_url` and settles.

**Reuse direct: no re-invention.** All the CAS discipline, sweep loops, worker-heartbeat tolerance, backoff math, admission control, and contribution-policy hot-reload defined in async-fleet-dispatch Sections "Core Primitives / Peer Side / Dispatcher Side / Operational Policy" apply to market dispatch with renamed field sets. Phase 2 implementation MUST NOT re-derive these — they are shared scaffolding.

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

**`compute_market_surface`**
- Args: `model_id: Option<String>` (optional filter).
- Flow: GET from Wire `/api/v1/compute/market-surface?model_id=...`. Returns per-model aggregation of active offers, pricing ranges, queue depths, provider counts, network-observed performance medians. Consumed by frontend §IV `ComputeMarketSurface.tsx`.

**`compute_market_enable` / `compute_market_disable`**
- Enable: set `is_serving = true`, start queue mirror loop, publish any configured offers.
- Disable: set `is_serving = false`, stop mirror loop, set all Wire offers to `inactive`.
- **Semantic note:** these IPCs toggle the runtime `is_serving` flag on `ComputeMarketState` — they do NOT modify `compute_participation_policy.allow_market_visibility`. Operator intent (the policy contribution) is durable; `is_serving` is the runtime on/off for the mirror loop. A node with `allow_market_visibility = false` AND `is_serving = true` still will not publish — the policy gate takes precedence. The UX distinction: "turn market participation off permanently" (supersede contribution) vs "pause serving temporarily" (IPC toggle `is_serving`).

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
   - `slug`: `"market:compute"` (virtual slug for market work -- not a pyramid slug)
   - `batch_id`: the Wire `job_id` (groups this job's lifecycle)
   - `epoch_id`: current timestamp-based epoch
   - `step_name`: `"compute-serve"`
   - `primitive`: `"llm_call"`
   - `layer`: 0
   - `target_id`: the Wire `job_id`
   - `system_prompt`: extracted from `messages` JSONB
   - `user_prompt`: extracted from `messages` JSONB
   - `model_tier`: the requested model
   - `state`: `"previewed"` (market jobs skip the preview gate — see below)

   **P3 fix — preview gate is a no-op for provider-side market jobs.** The DADBEAR preview gate exists to enforce operator cost budgets before committing to paid work. For provider-side market jobs this is redundant: the Wire's matched price + deposit IS the cost gate, and the provider already accepted the offer by publishing it. If the preview gate ran normally, it would try to price the job in USD against the operator's local-inference budget — the wrong currency, against the wrong budget, for a job the provider is being PAID to run. The supervisor must treat `compute-market` as a slug for which `dadbear_preview.rs` short-circuits: either enter the work item directly at `"previewed"` as shown above, or have the preview gate detect `slug == "market:compute"` and pass-through without cost check. (The requester-side market dispatch is different — Phase 3 preview gate estimates credit cost against `max_market_cost_credits`, which IS meaningful. That path does run through preview.)

3. **Create work attempt** via `create_work_attempt` (existing function in `dadbear_supervisor.rs`):
   - Links to the work item
   - Tracks dispatch timing and outcome

4. **Hold check** (via DADBEAR holds projection):
   - Check for active holds on the `"market:compute"` slug: `frozen` (operator paused market), `breaker` (quality system flagged this node), `cost_limit` (credit balance too low to absorb potential settlement risk).
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
- For market work items (slug `"market:compute"`): transition to `failed`. The Wire's timeout sweep (`sweep_timed_out_compute_jobs`) will independently fail the job and refund the requester.
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
   - DADBEAR work item created in `dadbear_work_items` with `slug = "market:compute"`, `state = "previewed"` (per §V P3 fix — preview gate is a no-op for provider-side market jobs; work items enter directly at `"previewed"` to skip the USD-denominated cost preview that doesn't apply to paid market work)

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
| Theme 3a (fill sends prompts to Wire) | Resolved per SOTA model: the Wire acts as transient bootstrap relay (see architecture §III + DD-A/DD-B/DD-D). `fill_compute_job` now accepts `messages` in the request body for forwarding through the bootstrap relay; the Wire does not persist payloads (forward-then-forget). Post-relay-market, `callback_url` shifts to non-Wire destinations with no protocol change. |
| Theme 3b (select_relay_chain undefined) | `fill_compute_job` rejects `relay_count > 0` inline. `select_relay_chain` function deferred entirely to relay market plan per DD-J — no stub in Phase 2. |
| Theme 3c (0-relay flow unspecified) | Specified per architecture §III bootstrap mode: variable relay count from launch; launch-era 0-relay means "direct-with-plausible-deniability post-bootstrap" (not yet reachable while Wire is the only relay). Wire-as-bootstrap-relay fills the gap transiently until non-Wire relay capacity deploys. |
| Theme 4 (QueueEntry schema diverged) | Section III: actual QueueEntry struct from compute_queue.rs used verbatim. Plan's stale struct explicitly called out. |
| Theme 4 (model_id filter missing in RPCs) | Phase 2 migration list item 4 adds `AND model_id = v_job.model_id` to settle/fail/void queue-decrement UPDATEs. |
| Theme 4 (v_wire_platform_operator_id undeclared in fill) | Phase 2 migration list item 1 adds the operator resolution query (DD-K canonical predicate `h.released_at IS NULL`). |
| Theme 4 (duplicate operator resolution in settlement) | Phase 2 migration list item 4 removes the duplicate. |
| Theme 4 (no filled->executing transition) | Added `start_compute_job` RPC in Phase 2 migration list item 3 per DD-J. |
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
