# Wire Compute Market — Phase Seams & Integration

**Date:** 2026-04-15
**Purpose:** Defines how phases interact, what each phase must leave working for the next, and where integration failures cascade. This document prevents phases from passing in isolation but breaking at boundaries.
**Source plan:** `wire-compute-market-build-plan.md`
**Source audit:** `audit-2026-04-15-compute-market-phases-2-9.md`
**Phase 1 handoff:** `handoff-2026-04-15-compute-market-session.md`

---

## I. Phase Dependency Graph

```
Phase 1 (shipped) ──→ Phase 2 (Exchange) ──→ Phase 3 (Settlement) ──→ Phase 4 (Bridge) ──→ Phase 5 (Quality) ──→ Phase 6 (Intelligence)
                          │                        │                       │                      │                      │
                          │                        │                       │                      │                      │
    ┌─────────────────────┘                        │                       │                      │                      │
    │ Depends on Phase 1:                          │                       │                      │                      │
    │  - ComputeQueueManager.enqueue_local         │                       │                      │                      │
    │  - GPU processing loop (round-robin drain)   │                       │                      │                      │
    │  - Compute Chronicle (9 event types)         │                       │                      │                      │
    │  - DADBEAR work items + holds + supervisor   │                       │                      │                      │
    │  - Fleet routing (fleet-dispatch endpoint)   │                       │                      │                      │
    │  - Queue mirror fields on QueueEntry         │                       │                      │                      │
    │    (source, work_item_id, attempt_id)        │                       │                      │                      │
    │  - Wire tables: wire_compute_offers,         │                       │                      │                      │
    │    wire_compute_jobs, wire_compute_obs,       │                       │                      │                      │
    │    wire_compute_queue_state                   │                       │                      │                      │
    │  - Wire RPCs: match, fill, settle, fail,     │                       │                      │                      │
    │    void, sweep, deactivate, multiplier        │                       │                      │                      │
    │  - wire_market_rotator + functions            │                       │                      │                      │
    │  - System entities: agentwireplatform,        │                       │                      │                      │
    │    agentwiregraphfund                         │                       │                      │                      │
    │  - 5 economic_parameter seed contributions    │                       │                      │                      │
    │                                               │                       │                      │                      │
    ├─ IMPLICIT: ACK+async prerequisite ────────────┤                       │                      │                      │
    │  (Cloudflare 120s timeout affects all          │                       │                      │                      │
    │   market jobs; must ship before or with       │                       │                      │                      │
    │   Phase 3 requester integration)              │                       │                      │                      │
    │                                               │                       │                      │                      │
    └───────────────────────────────────────────────┘                       │                      │                      │
                                                                            │                      │                      │
                          ┌─────────────────────────────────────────────────┘                      │                      │
                          │ Depends on Phase 3:                                                    │                      │
                          │  - settle_compute_job tested end-to-end                                │                      │
                          │  - Result delivery webhook on requester                                │                      │
                          │  - Provider settlement reporting path                                  │                      │
                          │  - WireComputeProvider dispatch integration                            │                      │
                          │                                                                        │                      │
                          │ IMPLICIT: Observation aggregation function                             │                      │
                          │  (Phase 3 records observations; Phase 5 reads                          │                      │
                          │   aggregated reputation. If aggregation isn't                          │                      │
                          │   built in Phase 3, Phase 5 has no signal.)                            │                      │
                          └────────────────────────────────────────────────────────────────────────┘                      │
                                                                                                                          │
                                                    ┌─────────────────────────────────────────────────────────────────────┘
                                                    │ Depends on Phase 5:
                                                    │  - Reputation scores (for steward provider selection)
                                                    │  - Quality holds integrated with DADBEAR holds
                                                    │  - Observation aggregation views (hourly/daily/weekly)
                                                    │
                                                    │ IMPLICIT: Phase 6 = DADBEAR market intelligence,
                                                    │  NOT a new system. Three DADBEAR slugs:
                                                    │  market:compute, market:storage, market:relay.
                                                    │  Compiler mappings, observation sources,
                                                    │  result application paths — not new infrastructure.
                                                    └──────────────────────────────────────────────
```

### Critical Implicit Dependencies (easily missed)

| Dependency | Producer Phase | Consumer Phase | Risk if Missing |
|---|---|---|---|
| ACK+async result delivery | Phase 2 or 3 (TODO in server.rs) | Phase 3 (all market jobs) | Cloudflare 524 on any job >120s. Market unusable for large models. |
| `start_compute_job` RPC (filled->executing transition) | Phase 2 or 3 | Phase 3 settlement (checks `status='executing'`) | Settlement rejects ALL jobs. No completed jobs, no revenue. |
| `cancel_compute_job` RPC | Phase 2 or 3 | Phase 3 (requester cancellation path) | Requester can't cancel. Paid reservation + deposit stuck forever. |
| Observation aggregation function | Phase 3 | Phase 5 (reputation signals) | Phase 5 has no performance data to enforce quality. Blind. |
| `select_relay_chain` stub | Phase 2 | Phase 3 fill RPC | fill_compute_job crashes on relay_count > 0. Must stub to reject >0 at launch. |
| `requester_operator_id != provider_operator_id` check | Phase 2 matching | Phase 5 (reputation integrity) | Self-dealing inflates reputation. Phase 5 builds on poisoned data. |
| `cloud_relay` privacy indicator on bridge offers | Phase 4 | Phase 5 (quality enforcement) | Quality probes can't distinguish bridge vs local. Wrong thresholds applied. |

---

## II. Job Lifecycle State Machine (Cross-Phase)

```
                                                          Phase 3
                                   Phase 2              (or sweep)          Phase 3                    Phase 5
                                  ┌───────┐             ┌───────┐         ┌────────┐                  ┌──────────┐
 match_compute_job ──→ RESERVED ──→ FILLED ──→ EXECUTING ──→ COMPLETED ──→ SETTLED    ──→ CHALLENGED ──→ CLAWBACK
                          │            │           │              │                             │
                          │            │           │              └──→ FAILED ──→ (deposit       └──→ QUALITY_HOLD
                          │            │           │                    refunded)
                          │            │           │
                          └──→ VOID    │           └──→ TIMED_OUT (sweep → FAILED)
                          (unfilled    │
                           reaches     └──→ CANCELLED
                           front)       (requester
                                        cancels)
```

### Transition Detail

| # | From | To | Trigger Actor | Trigger RPC/Endpoint | Built In Phase | Data Flows With Transition | On Failure |
|---|------|-----|--------------|---------------------|----------------|---------------------------|------------|
| T1 | (none) | RESERVED | Requester (via Wire) | `match_compute_job` RPC, called from `POST /api/v1/compute/match` | Phase 1 migration (RPC exists). Phase 2 wires the API route + node-side call. | Reservation fee debited from requester. Job row created. Queue depth incremented. Rotator advanced for reservation. Job dispatched to provider node via tunnel URL. | Match fails (no provider, budget exceeded, queue full). Requester gets exception. No credits moved. |
| T2 | RESERVED | FILLED | Requester (via Wire) | `fill_compute_job` RPC, called from `POST /api/v1/compute/fill` | Phase 1 migration (RPC exists). Phase 2/3 wires the node-side call + relay chain. | Token deposit debited from requester. Input token estimate recorded. Relay chain selected (if relay_count > 0). Provider gets fill notification with encrypted prompt via relay chain. | Fill fails (insufficient balance, job already voided). Requester gets exception. Reservation fee already non-refundable. |
| T3 | FILLED | EXECUTING | Provider node | **NOT YET DEFINED.** Needs `start_compute_job` RPC or node-side status update pushed to Wire. The GPU loop on the provider must signal "I started this job." | **Must be built in Phase 2 or 3.** This is the audit finding: settle checks for `status='executing'` but nothing produces it. | `dispatched_at` timestamp set. Chronicle event `market_started`. DADBEAR work item status updated. | If the provider never signals start, the job sits in FILLED until timeout sweep catches it. Sweep calls fail_compute_job, deposit refunded. |
| T4 | EXECUTING | COMPLETED | Provider node | `settle_compute_job` RPC, called from `POST /api/v1/compute/settle` (provider reports completion) | Phase 1 migration (RPC exists). Phase 3 wires the provider-side settlement reporting. | Actual cost calculated from measured tokens. Rotator determines recipient (provider 76/80, Wire 2/80, GF 2/80). Provider paid. Requester refunded overage (or Wire absorbs underage). Observation recorded. Queue depth decremented. Chronicle event `market_settled`. | Settlement RPC failure: job stays EXECUTING. **Critical cascade:** Provider did the work but isn't paid. Must retry. Phase 3 must specify local settlement retry queue. |
| T5 | RESERVED | VOID | Provider node (GPU loop) | `void_compute_job` RPC, called from `POST /api/v1/compute/void` | Phase 1 migration (RPC exists). Phase 2 wires the GPU loop void path. | No deposit to refund (none was charged). Reservation fee stays with provider. Queue depth decremented. Chronicle event `market_voided`. | If void report fails, the reservation stays in queue. Timeout sweep will eventually catch it via T7. |
| T6 | RESERVED or FILLED | CANCELLED | Requester | `cancel_compute_job` RPC, called from `POST /api/v1/compute/cancel` | **Must be built in Phase 2 or 3.** Audit finding: RPC not defined. | Deposit refunded to requester (if filled). Reservation fee stays with provider. Queue depth decremented. Provider notified to discard job. Chronicle event `market_cancelled`. | If cancel fails, job continues to execute. Not a critical failure (job completes normally, requester just can't abort). |
| T7 | EXECUTING or FILLED | FAILED | Wire (timeout sweep) OR provider (error report) | `fail_compute_job` RPC, called from `sweep_timed_out_compute_jobs` or `POST /api/v1/compute/fail` | Phase 1 migration (RPC exists). Phase 2/3 wires the timeout sweep scheduling + provider error reporting. | Deposit refunded to requester. Reservation fee stays with provider. Failure observation recorded (0 tokens, 0 latency — impacts provider metrics). Queue depth decremented. Chronicle event `market_failed`. | If fail RPC itself fails: job stays in limbo. Timeout sweep is idempotent and retries on next cycle (SKIP LOCKED prevents contention). |
| T8 | COMPLETED | CHALLENGED | Requester (via challenge infrastructure) | Challenge panel submission (Pillar 24 existing infrastructure extended to compute) | Phase 5 | Challenge evidence submitted (timing anomaly, quality evidence per privacy-respecting protocol). Challenge stake locked (proportional to job actual_cost). DADBEAR breaker hold placed on provider's market participation. | Challenge submission failure: requester retries. No credits at risk until challenge is accepted. |
| T9 | CHALLENGED | CLAWBACK | Challenge panel resolution | `clawback_compute_job` RPC (Phase 5 — new, not yet defined) | Phase 5 | Provider's earned credits debited. Graph Fund treatment determined. Challenger bounty paid from clawed-back amount. Provider reputation scored. DADBEAR breaker hold cleared or escalated. | Clawback from provider with insufficient balance: negative balance allowed? Or capped at available? Phase 5 must define. |
| T10 | CHALLENGED | (challenge dismissed) | Challenge panel resolution | Challenge panel adjudication | Phase 5 | Challenger's stake forfeited to challenged provider. Provider reputation unaffected (or slightly improved). DADBEAR breaker hold cleared. | Panel resolution failure: challenge stays open. Both parties' stakes locked indefinitely. Need timeout/default-dismiss. |

### Missing Transitions That Must Be Added

1. **FILLED -> EXECUTING (`start_compute_job`):** The plan's settlement RPC checks `status='executing'` but no transition produces this state. Either: (a) add a `start_compute_job` RPC called by the provider when GPU processing begins, or (b) have the API route handler that dispatches to the provider set the status. Option (a) is cleaner because the provider knows when GPU actually starts (not when the dispatch was sent).

2. **Cancel RPC (`cancel_compute_job`):** Must handle both RESERVED (no deposit, just remove from queue) and FILLED (refund deposit, remove from queue, notify provider to discard). Cannot cancel EXECUTING jobs (too late, GPU is running).

---

## III. Migration Ordering

### Phase 1 (SHIPPED — already applied to production Wire)

Two migrations applied in sequence:

**Migration 1: `20260414100000_market_prerequisites.sql`**
- `wire_market_rotator` table (per-node rotator arm state)
- `advance_market_rotator()` function (UPSERT with Bjorklund wrapping)
- `market_rotator_recipient()` function (76/2/2 Bjorklund distribution)
- `wire_graph_fund.source_type` CHECK extended with 5 market values
- `agentwireplatform` system entity (api_client -> operator -> agent -> handle)
- `agentwiregraphfund` system entity (api_client -> operator -> agent -> handle)
- 5 `economic_parameter` seed contributions (rotator config, deposit config, output estimate, staleness thresholds, relay performance floor)

**Migration 2: `20260414200000_compute_market_tables.sql`**
- `wire_compute_offers` table (with `provider_type` column — already has 'local'|'bridge')
- `wire_compute_jobs` table (NO messages column — relay-first privacy)
- `wire_compute_observations` table (append-only performance data)
- `wire_compute_queue_state` table (per-node, per-model mirror)
- `compute_queue_multiplier_bps()` function (IMMUTABLE, integer basis points)
- `settle_compute_job()` RPC (full settlement with rotator arm)
- `match_compute_job()` RPC (exchange matching with FOR UPDATE locking, race guard)
- `fill_compute_job()` RPC (deposit charging, no prompt data)
- `fail_compute_job()` RPC (deposit refund, failure observation)
- `void_compute_job()` RPC (unfilled reservation cleanup)
- `sweep_timed_out_compute_jobs()` function (SKIP LOCKED batch timeout)
- `deactivate_stale_compute_offers()` function (reads staleness from contribution)
- All tables: RLS enabled, GRANT to service_role
- All RPCs: SECURITY DEFINER, GRANT EXECUTE to service_role
- Indexes on all tables (model, node, provider, requester, batch, timeout)

**Migration 3: `20260415100000_node_identity.sql`**
- Node handle paths (`@playful/behem`, `@playful/mac-lan`)
- `wire_nodes.operator_id` direct ownership
- Registration by `(operator_id, node_handle)`

### Phase 2 (Exchange) — Wire-Side Migrations Needed

The tables and core RPCs already exist from Phase 1 migrations. Phase 2 Wire work is primarily **API route creation** and **heartbeat extension**, not new migrations.

New migration(s) required:
1. `start_compute_job()` RPC — transitions FILLED -> EXECUTING. Sets `dispatched_at = now()`. The settle RPC checks for this status.
2. `cancel_compute_job()` RPC — transitions RESERVED|FILLED -> CANCELLED. Refunds deposit if filled. Decrements queue depth.
3. Read `select_relay_chain` stub — returns empty result set, rejects relay_count > 0 with informative error.
4. Self-dealing guard: ALTER `match_compute_job` to add `AND v_offer.operator_id != p_requester_operator_id` in the offer selection query.

**FK Dependency:** None new. Phase 2 migrations only add RPCs that reference Phase 1 tables (already exist).

### Phase 3 (Settlement) — Wire-Side Migrations Needed

No new tables. RPCs already exist (settle, fail, void from Phase 1).

New migration(s) required:
1. Observation aggregation function: `aggregate_compute_observations(p_node_id, p_model_id)` — computes median, p25, p75, p95 across time horizons. Updates `wire_compute_offers.observed_*` columns. Can be called from settlement or on schedule.
2. Update `fill_compute_job` return type if relay chain routing info needs to be added (currently returns deposit_charged and estimated_output_tokens; plan spec returns relay_chain + provider_ephemeral_pubkey — but these are Wire-internal, not persisted, so handled in the API route, not the RPC).

**FK Dependency:** `wire_compute_observations.job_id` references `wire_compute_jobs.id` (both Phase 1 — already exist). Phase 3 populates observations via the settlement path. No new FK constraints.

### Phase 4 (Bridge) — Wire-Side Migrations Needed

`provider_type` column already exists on `wire_compute_offers` (from Phase 1 CREATE TABLE: `DEFAULT 'local'`, values `'local' | 'bridge'`). No ALTER needed.

New migration(s) required:
1. Add `bridge_dollar_cost` column to `wire_compute_jobs` — optional INTEGER (cents), tracks OpenRouter dollar cost for bridge jobs. NULL for local GPU jobs.
2. Add `bridge_openrouter_model` column to `wire_compute_jobs` — optional TEXT, the actual OpenRouter model slug used.
3. Alter `wire_compute_offers.privacy_capabilities` CHECK or default to include `'cloud_relay'` for bridge offers.

**FK Dependency:** None new. Bridge columns are nullable additions to existing Phase 1 table.

### Phase 5 (Quality) — Wire-Side Migrations Needed

New tables required:
1. `wire_compute_challenges` — challenge records referencing `wire_compute_jobs.id`
2. `wire_compute_challenge_stakes` — locked stakes for challengers and challenged providers
3. `wire_compute_reputation` — aggregated reputation scores per (node_id, model_id)

New RPCs required:
1. `clawback_compute_job()` — debit provider, pay challenger bounty, handle negative balance
2. `file_compute_challenge()` — create challenge, lock stake, place DADBEAR breaker hold
3. `resolve_compute_challenge()` — adjudicate, distribute stakes, update reputation
4. `aggregate_compute_reputation()` — recompute reputation from challenge outcomes + observations

**FK Dependency:** `wire_compute_challenges.job_id` references `wire_compute_jobs.id` (Phase 1). `wire_compute_challenge_stakes.challenge_id` references `wire_compute_challenges.id` (same Phase 5 migration). `wire_compute_reputation.node_id` references `wire_nodes.id` (pre-existing).

### Phase 6 (Intelligence) — Wire-Side Migrations Needed

New migration(s) required:
1. `steward_publication` contribution type — add to contribution type constraints or type registry
2. `config_recommendations` RPC or function — returns SOTA configs matching hardware profile
3. Subscription tables for steward publication following (or reuse existing subscription infra if it exists)

**FK Dependency:** Steward publications are standard `wire_contributions` rows. No new FK constraints beyond the existing contribution schema.

### Complete Migration Sequence

```
ALREADY APPLIED:
  20260414100000_market_prerequisites.sql      ← rotator, entities, seeds
  20260414200000_compute_market_tables.sql     ← 4 tables, 8 RPCs, indexes
  20260415100000_node_identity.sql             ← node handles, operator_id

PHASE 2 (must apply before Phase 2 node work):
  2026MMDD_phase2_exchange_rpcs.sql            ← start_compute_job, cancel_compute_job,
                                                  select_relay_chain stub,
                                                  match_compute_job self-dealing guard

PHASE 3 (must apply before Phase 3 node work):
  2026MMDD_phase3_observation_aggregation.sql  ← aggregate_compute_observations function

PHASE 4 (must apply before Phase 4 node work):
  2026MMDD_phase4_bridge_columns.sql           ← bridge_dollar_cost, bridge_openrouter_model,
                                                  privacy_capabilities update

PHASE 5 (must apply before Phase 5 node work):
  2026MMDD_phase5_quality_tables.sql           ← wire_compute_challenges,
                                                  wire_compute_challenge_stakes,
                                                  wire_compute_reputation
  2026MMDD_phase5_quality_rpcs.sql             ← clawback_compute_job,
                                                  file_compute_challenge,
                                                  resolve_compute_challenge,
                                                  aggregate_compute_reputation

PHASE 6 (must apply before Phase 6 node work):
  2026MMDD_phase6_intelligence.sql             ← steward_publication type,
                                                  config_recommendations,
                                                  subscription infra
```

---

## IV. Per-Seam Analysis

### Seam 1->2: Phase 1 (shipped) -> Phase 2 (Exchange)

**What Phase 1 leaves working:**

Node-side (Rust):
- `compute_queue.rs`: `ComputeQueueManager` with `enqueue_local()`, `dequeue_next()` (round-robin), snapshot methods
- `QueueEntry` struct with `source` field (currently `"local"` or `"fleet_received"`), `work_item_id`, `attempt_id`, `job_path`, `chronicle_job_path`
- GPU processing loop in `main.rs` draining queues round-robin
- Fleet dispatch endpoint (`/v1/compute/fleet-dispatch`) with JWT authentication
- Compute Chronicle with 9 event types and DADBEAR correlation columns
- DADBEAR canonical architecture: compiler, supervisor, work items, holds, preview gate
- `QueueLiveView.tsx` showing real-time queue state
- `ComputeChronicle.tsx` with event display

Wire-side (Postgres):
- All 4 compute market tables with indexes and RLS
- All 8 RPCs (match, fill, settle, fail, void, sweep, deactivate, multiplier)
- Rotator arm infrastructure (table + 2 functions)
- System entities (agentwireplatform, agentwiregraphfund)
- 5 economic parameter seed contributions

**What Phase 2 must build:**

Node-side:
1. `enqueue_market()` method on `ComputeQueueManager` — new method accepting market jobs. Must check `max_market_depth` policy limit. Must set `source: "market_received"` on the QueueEntry.
2. `/v1/compute/job-dispatch` endpoint in `server.rs` — receives matched jobs from Wire, verifies `wire_job_token` JWT, calls `enqueue_market()`. This endpoint already has a fleet-dispatch sibling; follow the same pattern.
3. Wire mirror push loop — on every queue state change, POST snapshot to `POST /api/v1/compute/queue-state`. The `QueueSnapshot` struct maps to the `wire_compute_queue_state` schema. Must include `seq` (monotonically increasing per node+model) for staleness rejection.
4. Offer management IPC commands — `compute_offer_create`, `compute_offer_update`, `compute_offer_remove`. These call Wire API routes.
5. Chronicle: add `market_received` and `market_offered` event write points. Chronicle columns (`work_item_id`, `attempt_id`) already exist.
6. DADBEAR integration: market jobs received by provider create DADBEAR work items with `source: "market_received"`. This parallels fleet_received work items.

Wire-side:
1. API routes: `POST /api/v1/compute/offers`, `POST /api/v1/compute/match`, `POST /api/v1/compute/fill`, `POST /api/v1/compute/queue-state`, `GET /api/v1/compute/market-surface`
2. Heartbeat extension: `compute_market` section in heartbeat response
3. `start_compute_job` RPC (FILLED -> EXECUTING transition)
4. `cancel_compute_job` RPC (RESERVED|FILLED -> CANCELLED)
5. `select_relay_chain` stub (reject relay_count > 0)
6. Self-dealing guard on match_compute_job

Frontend:
1. `ComputeOfferManager.tsx` — create/edit offers
2. `ComputeMarketSurface.tsx` — browse network providers
3. `QueueLiveView.tsx` update — show both local and market entries

**Integration risk: Semaphore re-introduction.** The global semaphore was removed in Phase 1. The queue is the sole serializer. Phase 2 must NOT introduce any form of semaphore, mutex, or secondary serialization for market jobs. Market entries and local entries share the same FIFO queue and the same GPU loop. The only difference is QueueEntry.source and whether credits flow.

**Integration risk: Queue mirror race.** The mirror push must use monotonic `seq` per (node_id, model_id). Wire rejects pushes where seq <= current. But if the node pushes seq=5, then the GPU loop completes a job and pushes seq=6, and seq=5 arrives after seq=6 due to network reordering, the Wire correctly rejects seq=5. This is correct behavior. But the node must not assume push success. Fire-and-forget with seq monotonicity is sufficient.

**CONTRACT:** Phase 1 leaves `QueueEntry.source` as a plain String. Phase 2 must add a new source value `"market_received"` but must NOT change the type from String to an enum without updating all existing call sites (fleet_received, local, stale_check). The GPU loop reads `source` for chronicle event emission and does not branch on it for execution — market and local jobs execute identically through the unified LLM path.

### Seam 2->3: Phase 2 (Exchange) -> Phase 3 (Settlement)

**What Phase 2 leaves working:**
- Provider can receive and process market jobs via `/v1/compute/job-dispatch`
- Wire has matching and fill RPCs wired to API routes
- Queue mirror is active (node pushes state, Wire stores it)
- Market jobs go through DADBEAR on provider side
- Offer management (create/update/remove) works
- Market surface is browsable
- BUT: no requester integration (requester can't dispatch TO the market)
- BUT: no settlement reporting (provider processes job but doesn't report back to Wire)
- BUT: no result delivery (requester has no way to receive results)

**What Phase 3 must build:**

Node-side (requester):
1. `WireComputeProvider` in the dispatch chain. Add `wire-compute` as a recognized provider in `chain_dispatch.rs`. When dispatch policy resolves to `wire-compute`, delegate to `WireComputeProvider.call()`. This is a new branch in the dispatch logic alongside existing `ollama-local` and `openrouter`.
2. `/v1/compute/result-delivery` webhook endpoint on requester node. Wire pushes completed results here. Resolves a oneshot channel registered by `WireComputeProvider.call()`.
3. DADBEAR integration on requester side: outbound market calls create DADBEAR work items going through the preview gate (cost estimation via market pricing). Crash recovery: DADBEAR supervisor can resume awaiting market calls after restart.

Node-side (provider):
1. Settlement reporting: GPU loop completion path must POST settlement metadata to `POST /api/v1/compute/settle` (token counts, latency, finish reason). This is the "report_completion_to_wire" call in the GPU loop's Market branch.
2. ACK+async result delivery: provider must ACK job receipt immediately (HTTP 200), process async, POST result back to Wire (which forwards to requester). Without this, Cloudflare 524 kills any job >120s.

Wire-side:
1. `POST /api/v1/compute/settle` route wired to `settle_compute_job` RPC
2. `POST /api/v1/compute/void` route wired to `void_compute_job` RPC
3. `POST /api/v1/compute/fail` route wired to `fail_compute_job` RPC
4. Observation aggregation function (phase-critical: Phase 5 depends on this data)
5. Wire-side result forwarding: after settlement, Wire pushes result to requester's tunnel URL via `/v1/compute/result-delivery`

**Integration risk: Double-dispatch.** Fleet routing happens BEFORE wire-compute in the dispatch chain. `WireComputeProvider` must only be reached when fleet dispatch returns None (no fleet capacity). If fleet and wire-compute both try to handle the same call, the call executes twice (once on fleet, once on market — double GPU time, double credits). The dispatch chain must be: cache -> route resolution -> fleet dispatch -> (if None) wire-compute -> (if None) local queue.

**Integration risk: Privacy violation in fill.** The plan's `WireComputeProvider.fill_job()` code sketch sends `system_prompt, user_prompt` as parameters. But the actual `fill_compute_job` RPC (already deployed) accepts only `p_input_token_estimate` and `p_temperature` / `p_max_tokens` — NOT prompts. The Wire never sees payloads. The fill call sends token counts; the actual prompt flows through the relay chain (or direct to provider for 0-relay launch). Implementers must follow the deployed RPC signature, not the plan's code sketch.

**CONTRACT:** Phase 2 must leave the provider-side GPU loop with a clear extension point at the Market job completion path. Currently the GPU loop processes the job and returns via oneshot. Phase 3 adds: after GPU completion for Market source jobs, call `report_completion_to_wire(job_id, prompt_tokens, completion_tokens, latency_ms, finish_reason)`. Until Phase 3: provider processes market jobs but result delivery is synchronous (HTTP response to the dispatch request). Phase 3 switches to async (ACK immediately, POST result later).

### Seam 3->4: Phase 3 (Settlement) -> Phase 4 (Bridge)

**What Phase 3 leaves working:**
- Full credit loop for local-GPU providers (match -> fill -> execute -> settle -> credits flow)
- Requester integration via `WireComputeProvider` in dispatch chain
- Settlement, fail, void, cancel RPCs wired and tested end-to-end
- DADBEAR integration on both requester and provider sides
- ACK+async result delivery eliminating Cloudflare 524 timeouts
- Observation recording and aggregation

**What Phase 4 must build:**

Node-side:
1. Bridge handler: a VARIANT of the provider-side handler, not a new system. Receives market job -> instead of routing to local GPU, dispatches to OpenRouter -> returns result via same settlement reporting path.
2. Bridge-specific DADBEAR work items with `source: "bridge"`.
3. Bridge cost tracking: `bridge_dollar_cost` recorded per job for margin visibility.
4. OpenRouter error classification table: maps HTTP status codes to Wire job states.
   - 200: COMPLETED (settle normally)
   - 429: FAILED (rate limited — retry with backoff or fail)
   - 503: FAILED (service unavailable)
   - 402: FAILED + SUSPEND ALL BRIDGE OFFERS (insufficient OpenRouter funds)
   - 400: FAILED (bad request — likely model mismatch)
   - timeout: FAILED (provider timeout — Cloudflare or OpenRouter)
5. Bridge-dedicated OpenRouter API key (separate from personal use key to prevent rate limit interference).
6. Model lifecycle management: periodic check against OpenRouter `/api/v1/models`, diff against active bridge offers, deactivate offers for deprecated/removed models.
7. Chronicle: `bridge_dispatched`, `bridge_returned`, `bridge_failed`, `bridge_cost_recorded` event types.

Wire-side:
1. Migration: `bridge_dollar_cost` and `bridge_openrouter_model` columns on `wire_compute_jobs`.
2. Bridge offers must carry `privacy_capabilities: '{cloud_relay}'` instead of `'{standard}'`. Requesters with strict privacy policies can filter out bridge providers.
3. Dollar cost tracking in settlement metadata (optional field, not in credit flow).

**Integration risk: Two external calls.** Bridge adds a second external HTTP call (to OpenRouter) inside the provider-side job handler. The existing local-GPU path makes zero external calls (GPU is local). Error handling must be completely isolated — an OpenRouter failure must not corrupt the local-GPU code path. Use separate handler branches, not a shared handler with conditional logic.

**Integration risk: Fleet vs bridge ordering.** Same-operator fleet routing could route own builds through bridge (paying OpenRouter dollars for own inference). Fleet dispatch must prefer local-GPU fleet nodes over bridge fleet nodes. The fleet roster should carry `provider_type` per model so the fleet dispatch can prefer `local` over `bridge`.

**Integration risk: Rate limit isolation.** Personal builds and bridge jobs sharing one OpenRouter API key means bridge traffic can exhaust server-side rate limits, blocking personal builds. Settlement layer has programmatic key provisioning. Phase 4 must use a bridge-dedicated key.

**CONTRACT:** Phase 3 must leave the provider-side settlement reporting flexible enough that Phase 4 can add bridge metadata alongside the credit settlement. Specifically: the settlement report to Wire should accept optional `bridge_dollar_cost` and `bridge_openrouter_model` fields. The `POST /api/v1/compute/settle` route handler should forward these to the job row update even if they're NULL for local-GPU jobs.

### Seam 4->5: Phase 4 (Bridge) -> Phase 5 (Quality)

**What Phase 4 leaves working:**
- Bridge provider type working (receive market job -> OpenRouter -> settle)
- Dual-currency tracking (credits + dollars)
- Model lifecycle management for bridge offers
- Bridge offers tagged with `provider_type='bridge'` and `privacy_capabilities: '{cloud_relay}'`
- OpenRouter error classification driving correct Wire job state transitions

**What Phase 5 must build:**

Wire-side:
1. Challenge tables and RPCs (file, resolve, clawback)
2. Challenge staking (DD-9: economic gates, not rate limits)
3. Observation aggregation views (if not already built in Phase 3 — check)
4. Reputation scoring function from challenge outcomes + observations
5. Timing anomaly detection: flag physically implausible response times

Node-side:
1. Challenge submission path from requester's steward
2. Quality holds: upheld challenge places DADBEAR breaker hold on provider's market participation
3. Quality probes against BOTH provider types (local-GPU and bridge)
4. Bridge quality probes must test through the actual cloud relay path (not bypass to local GPU)

**Integration risk: Provider type conflation.** Bridge jobs have worse baseline quality variance than local GPU (OpenRouter may swap backends, different quantization). Phase 5's anomaly detection thresholds must differ per `provider_type`. A timing anomaly for a local-GPU provider (response faster than hardware could produce) is different from a timing anomaly for a bridge provider (response time includes network latency + OpenRouter queue time).

**Integration risk: Challenge evidence vs privacy.** Wire never sees payloads (by design). But challenge panels need evidence. Options: (a) requester opts into revealing prompt for challenge, (b) challenges limited to timing/metadata anomalies, or (c) hash-then-re-run protocol. Phase 5 must choose and implement one. This is an architectural decision, not a detail.

**Integration risk: No proactive detection.** Lazy providers (cached responses + artificial delay) are undetectable by timing analysis alone. The steward doesn't exist until Phase 6. Phase 5 must either: (a) extend existing honeypot infrastructure to compute (Wire dispatches known-answer test jobs at random intervals), or (b) accept that lazy providers are not detectable until Phase 6. Option (a) is strongly recommended.

**CONTRACT:** Phase 4 must leave bridge offers clearly tagged with `provider_type='bridge'` so Phase 5 can apply different quality thresholds per provider type. Phase 4 must also leave `bridge_dollar_cost` populated on bridge job rows so Phase 5 can verify that bridge costs are consistent with claimed inference (a bridge claiming to use a 70B model but paying dollar costs consistent with a 7B model is a quality signal).

### Seam 5->6: Phase 5 (Quality) -> Phase 6 (Intelligence)

**What Phase 5 leaves working:**
- Challenge infrastructure (file, resolve, clawback, staking)
- Reputation system affecting matching (low-reputation providers sorted lower)
- Observation aggregation function (hourly/daily/weekly stats)
- Quality holds integrated with DADBEAR holds (breaker holds block market dispatch)
- Timing anomaly detection flagging suspect jobs

**What Phase 6 must build:**

Phase 6 is NOT a new system. It is DADBEAR extended with market-domain observation sources, compiler mappings, and result application paths. Per the audit: Phases 6-9 collapse into one phase.

New observation sources:
1. Heartbeat demand signal extractor — reads unfilled bid counts, model popularity
2. Chronicle health monitor — reads throughput drift, failure rates
3. Network config fetcher — reads SOTA configs from Wire contributions

New compiler mappings:
1. `demand_signal -> model_portfolio_eval` — decide which models to load
2. `throughput_drift -> pricing_adjustment` — adjust rates based on utilization
3. `queue_utilization -> market_depth_adjustment` — expand/contract market depth

New result application paths:
1. Call Ollama control plane (load/unload/swap models)
2. Supersede pricing contributions (new rates replace old)
3. Publish experiment results as Wire contributions

**Integration risk: Quality holds block experiments.** Phase 6's auto-adjust could fight with Phase 5's quality holds. If the auto-pricing work item tries to increase pricing (because queue is deep), but a quality hold is about to freeze the node, the pricing increase is wasted. DADBEAR holds prevent this IF properly implemented: a breaker hold blocks dispatch of ALL non-essential work items for the held scope. Phase 5 must scope holds correctly (per-model, not global) so unaffected models can still be adjusted.

**Integration risk: Stale data after hold clear.** When a quality hold clears (challenge resolved), observation data accumulated during the hold is stale (no market traffic during hold). Phase 6 must trigger a fresh observation cycle before resuming experiments.

**Integration risk: GPU access for management LLM calls.** The compute queue is the sole serializer. A sentinel 2b model call sits behind market jobs in the queue. Phase 6 needs either: (a) management-class queue bypass (sentinel jobs skip the FIFO), or (b) dedicated small-model VRAM reservation (sentinel always has its own GPU slot). This is an architecture decision that must be resolved before Phase 6 implementation.

**CONTRACT:** Phase 5 must leave the quality hold mechanism properly scoped per (node_id, model_id) and integrated with DADBEAR holds so Phase 6's work items respect them. When a hold clears, Phase 5 must emit a DADBEAR observation event (`hold_cleared`) that Phase 6's compiler can map to "run fresh observation cycle before resuming experiments."

---

## V. Cross-Phase Data Flows

| Data Entity | Created In | Populated/Written In | Consumed In | How It Crosses Phase Boundary |
|---|---|---|---|---|
| `wire_compute_offers` row | Phase 1 (table), Phase 2 (API route + node calls) | Phase 2 (offer create/update), Phase 4 (bridge offers), Phase 6 (steward pricing) | Phases 2-6 (matching, display, quality, intelligence) | Direct Wire-side table reference. Offer rows are long-lived, updated in place. |
| `wire_compute_jobs` row | Phase 1 (table), Phase 2 (match creates row) | Phase 2 (match), Phase 3 (fill, settle, fail, void, cancel), Phase 4 (bridge columns) | Phases 3 (settlement), 5 (challenges, observations), 6 (demand analysis) | Direct Wire-side table reference. Job row accumulates state across its lifecycle. |
| `wire_compute_observations` row | Phase 1 (table), Phase 3 (settle writes observations) | Phase 3 (on every settlement) | Phase 5 (reputation scoring), Phase 6 (performance analysis) | Direct Wire-side table reference. Append-only. Phase 5 reads aggregated views. |
| `wire_compute_queue_state` row | Phase 1 (table), Phase 2 (mirror push writes) | Phase 2 (mirror push loop), continuously updated | Phase 2 (matching uses queue depth), Phase 6 (utilization analysis) | Node pushes to Wire via `POST /api/v1/compute/queue-state`. Wire stores latest per (node, model). Staleness checked at match time (2-min cutoff from `staleness_thresholds` contribution). |
| DADBEAR work items | Phase 1 (DADBEAR architecture) | Phase 2 (provider side), Phase 3 (requester side), Phase 4 (bridge source) | All phases | Local SQLite on each node. Work items have `source` field distinguishing `local`, `market_received`, `fleet_received`, `bridge`. DADBEAR supervisor recovers in-flight items on restart. |
| Chronicle events | Phase 1 (Chronicle architecture, 9 types) | Phase 2 (+market_received, market_offered), Phase 3 (+market_settled, market_failed, market_voided), Phase 4 (+bridge_*) | Phase 5 (timing evidence), Phase 6 (health monitoring) | Local SQLite `pyramid_compute_events` table. 17 columns, 6 indexes. Events carry `work_item_id` and `attempt_id` for DADBEAR correlation. |
| Reputation scores | N/A | Phase 5 (computed from challenges + observations) | Phase 6 (steward provider selection) | Wire-side computed views or `wire_compute_reputation` table. Phase 6 reads via API. |
| Quality holds | N/A | Phase 5 (placed on challenge, cleared on resolution) | Phase 6 (blocks work item dispatch) | Two paths: (1) Wire-side offer status change (active -> held), propagated to node via heartbeat. (2) DADBEAR breaker hold placed locally, blocking dispatch. Both paths must agree. |
| Steward publications | N/A | Phase 6 (published as Wire contributions) | Phase 6 (other nodes read via subscription) | Standard `wire_contributions` rows with `type: 'steward_publication'`. Cross-node via Wire query/subscription. |
| Economic parameter contributions | Phase 1 (5 seeds) | Phase 6 (steward supersedes parameters) | All phases (rotator reads slot counts, match reads staleness, fill reads deposit config) | Wire-side `wire_contributions` with `type: 'economic_parameter'`. Supersedable. All RPCs read latest active contribution at call time. |

---

## VI. Failure Mode Cascades

### Queue Mirror Failure (Phase 2)

**Trigger:** Node stops pushing queue state to Wire (network issue, bug in mirror loop, node crash).

**Cascade:**
1. Wire holds stale queue state for the affected node+model(s).
2. `match_compute_job` has a 2-minute staleness check: `q.updated_at > now() - interval '2 minutes'`. After 2 minutes of no push, the node's offers stop matching.
3. BUT: within the 2-minute window, the Wire may match jobs to a node whose queue is fuller than reported. Matched jobs arrive at a queue that may be at capacity.
4. Provider rejects the job (queue full) -> job fails -> requester's deposit is refunded but reservation fee is lost -> requester paid for nothing useful.
5. If many jobs are matched against stale state, a burst of failures degrades the provider's observation metrics.
6. `deactivate_stale_compute_offers` (5-minute threshold from `staleness_thresholds` contribution) eventually marks all offers offline.

**Mitigation:** Node pushes full snapshot on reconnect (not just delta). The `seq` monotonic counter prevents stale pushes from overwriting fresh state on reconnect race. But the 2-minute window allows some stale matches. This is acceptable: reservation fees are small, and the failure observation correctly penalizes unreliable nodes.

### Settlement Failure (Phase 3)

**Trigger:** Provider completes GPU work, tries to POST settlement to Wire, Wire is unreachable.

**Cascade:**
1. Job stays in EXECUTING status. Provider has the result but can't settle.
2. Requester is waiting on result delivery (oneshot channel). Timeout will fire.
3. Timeout sweep marks job as FAILED. Deposit refunded to requester.
4. BUT: provider did the GPU work and will never be paid for it.
5. Result is lost (transiently held by provider, never delivered to requester).
6. If Wire connectivity is intermittent, provider accumulates unpaid completed jobs.

**Mitigation:** Phase 3 must implement a local settlement retry queue on the provider node. When settlement POST fails, the settlement metadata (job_id, tokens, latency, finish_reason) is persisted to a local file/SQLite table. On Wire reconnection, the retry queue replays all pending settlements. The timeout sweep must be generous enough to allow retry (timeout_at is set at match time based on estimated queue time — typically minutes, not seconds). DADBEAR work items provide the durability layer: the work item stays in `dispatched` state until settlement succeeds, and the supervisor replays it on restart.

### Bridge OpenRouter Failure (Phase 4)

**Trigger:** Bridge job dispatches to OpenRouter, OpenRouter returns error.

**Cascade depends on error type:**
- **429 (rate limited):** Bridge retries with backoff. If retry succeeds, job completes normally. If retries exhaust, job fails (deposit refunded, reservation stays with bridge operator). Bridge operator loses the OpenRouter cost of partial attempts.
- **503 (service unavailable):** Same as 429.
- **402 (insufficient funds):** ALL bridge offers must be immediately suspended (not just this model). The operator's OpenRouter account is empty. Continuing to accept bridge jobs will produce 100% failure rate. `deactivate_stale_compute_offers` won't catch this because the node is still alive and pushing heartbeats. Phase 4 must add a "suspend all bridge offers" action triggered by 402.
- **400 (bad request):** Usually a model mismatch (model deprecated on OpenRouter while offer was active). Deactivate the specific offer for this model. Trigger model lifecycle refresh.
- **timeout:** Bridge adds relay + OpenRouter latency. With ACK+async, the provider has already ACK'd. The async result delivery times out. Job fails (deposit refunded).

**Cascade to Phase 5:** Frequent bridge failures feed into observation metrics. If a bridge provider has consistently worse metrics than local-GPU providers, Phase 5's reputation system naturally deprioritizes it. No special handling needed — the observation data tells the story.

### Quality Hold Cascade (Phase 5 -> Phase 6)

**Trigger:** Phase 5 places a quality hold on a provider (challenge filed, evidence pending).

**Cascade:**
1. DADBEAR breaker hold blocks dispatch of all market work items for the held node+model.
2. Wire-side: offer status changes from `active` to something that prevents matching (e.g., `held`). New jobs stop being matched.
3. In-flight jobs continue executing (can't cancel mid-GPU). Their settlements proceed normally.
4. Phase 6 market intelligence can't dispatch work items for the held node. Experiments stall.
5. If hold duration is long (challenge adjudication takes time), observation data for the held period is absent. When the hold clears, Phase 6's steward has stale data.
6. Steward decisions based on stale data when hold clears could be wrong (e.g., prices set for demand that no longer exists).

**Mitigation:** On hold clear, Phase 5 must emit a DADBEAR observation event. Phase 6's compiler should map `hold_cleared` to "run fresh observation cycle: push queue state, check demand signals, verify pricing before resuming experiments." The fresh cycle runs before any pricing/portfolio changes take effect.

### Requester Crash Mid-Await (Phase 3)

**Trigger:** Requester's `WireComputeProvider.call()` is awaiting the result webhook. The app crashes or restarts.

**Cascade:**
1. The oneshot channel is dropped. If the result arrives via webhook, there's no receiver.
2. The DADBEAR work item for the outbound market call is in `dispatched` state.
3. On restart, DADBEAR supervisor finds the in-flight work item and attempts recovery.
4. Recovery must: check job status on Wire (is it completed? failed? still executing?). If completed, fetch the result. If failed, handle the failure. If still executing, re-register the oneshot channel and continue waiting.
5. The deposit is already charged. If the job completes but the result is lost, the requester paid for nothing.

**Mitigation:** DADBEAR work items are the durability layer. The work item's semantic path ID includes the job_id. On restart, the supervisor queries Wire for job status and either fetches the result (if completed) or re-registers for webhook delivery (if still in progress). Phase 3 must implement this recovery path as part of the DADBEAR integration.

### Estimation Subsidy Accumulation (Phase 3, systemic)

**Trigger:** Network output estimates are consistently too low (e.g., new model with longer outputs than historical median).

**Cascade:**
1. `fill_compute_job` calculates deposit from network median output. If median is low, deposit is low.
2. `settle_compute_job` calculates actual cost from measured tokens. Actual > deposit.
3. Wire platform operator (`agentwireplatform`) absorbs the difference. Its balance decreases.
4. If many jobs have low estimates, the platform operator's balance goes deeply negative.
5. The platform operator's negative balance is not rate-limited (by design — it absorbs estimation risk).
6. Sustained negative balance means the Wire platform is subsidizing compute at a loss.

**Mitigation:** Self-correcting: as observations accumulate for the new model, the network median output increases, and estimates improve. The 7-day observation window in the median calculation means correction happens within a week. The platform operator's balance is replenished from platform revenue (credit purchases, Graph Fund overflow). Phase 3 should add monitoring for platform operator balance — alert if it goes below a threshold (contribution-driven, of course).

---

## VII. Integration Test Points

### Seam 1->2 Tests (Phase 2 implementer verifies these)

- [ ] Market job arrives at provider `/v1/compute/job-dispatch`, creates QueueEntry with `source: "market_received"`
- [ ] Market QueueEntry goes through the same GPU loop and unified LLM path as local entries
- [ ] Queue mirror pushes to Wire after market enqueue, with correct `seq`, `total_depth`, `market_depth`
- [ ] Wire rejects stale queue mirror push (seq <= current)
- [ ] Wire queue mirror push on reconnect sends full snapshot (not delta)
- [ ] `match_compute_job` rejects match when queue mirror is stale (>2 min)
- [ ] `match_compute_job` rejects self-dealing (requester_operator_id = provider_operator_id)
- [ ] `start_compute_job` transitions FILLED -> EXECUTING, sets `dispatched_at`
- [ ] `cancel_compute_job` transitions RESERVED -> CANCELLED (no deposit to refund)
- [ ] `cancel_compute_job` transitions FILLED -> CANCELLED (deposit refunded)
- [ ] Chronicle records `market_received` with `work_item_id` and correct `source`
- [ ] DADBEAR work item created for received market job (crash recovery: restart recovers in-flight market job)
- [ ] Market job capacity check: `enqueue_market` rejects when `market_depth >= max_market_depth`
- [ ] Local enqueue is never rejected (blocks until capacity, never errors — matches old semaphore behavior)
- [ ] Concurrent match race: two matches for the same node+model, one wins, one gets "queue full" exception (race guard re-check after lock)

### Seam 2->3 Tests (Phase 3 implementer verifies these)

- [ ] End-to-end: requester match -> fill -> provider execute -> settle -> credits flow correctly (provider earns, requester refund, Graph Fund levy on rotator slots)
- [ ] Requester dispatch chain: cache -> fleet -> wire-compute -> local. Wire-compute only reached when fleet returns None.
- [ ] No double-dispatch: fleet exhausted -> wire-compute fallback -> market job created. Fleet dispatch does not also create a local queue entry.
- [ ] ACK+async: provider ACKs job-dispatch immediately (HTTP 200), processes async, POSTs result to requester's `/v1/compute/result-delivery`
- [ ] Requester crash mid-await -> restart -> DADBEAR supervisor recovers -> checks job status on Wire -> fetches result if completed
- [ ] Settlement retry: provider completes GPU work, settlement POST fails (Wire down), settlement metadata persisted locally, replayed on reconnect
- [ ] Settlement with actual > deposit: Wire platform absorbs difference, requester not charged extra, platform operator balance decremented
- [ ] Settlement with actual < deposit: requester refunded overage
- [ ] Rotator arm: over 80 settlements, provider receives ~76, Wire receives ~2, Graph Fund receives ~2
- [ ] `fill_compute_job` does NOT receive prompt data (only token count, temperature, max_tokens)
- [ ] Void path: unfilled reservation reaches queue front -> `void_compute_job` called -> no deposit refund (none was charged), reservation fee stays with provider
- [ ] Timeout sweep: job in FILLED/EXECUTING past `timeout_at` -> `sweep_timed_out_compute_jobs` -> `fail_compute_job` -> deposit refunded

### Seam 3->4 Tests (Phase 4 implementer verifies these)

- [ ] Bridge job uses exact same settlement reporting path as local-GPU job (same POST to settle endpoint, same RPC)
- [ ] Bridge job includes `bridge_dollar_cost` and `bridge_openrouter_model` in settlement metadata
- [ ] Bridge offer has `privacy_capabilities: '{cloud_relay}'`, not `'{standard}'`
- [ ] OpenRouter 402 -> ALL bridge offers suspended immediately (not just the failing model)
- [ ] OpenRouter 429 -> retry with backoff -> fail if retries exhaust (deposit refunded)
- [ ] Fleet dispatch prefers local-GPU fleet nodes over bridge fleet nodes for same model
- [ ] Bridge uses separate OpenRouter API key (not shared with personal builds)
- [ ] Model lifecycle: deprecated OpenRouter model -> bridge offer deactivated
- [ ] Bridge chronicle events: `bridge_dispatched`, `bridge_returned`, `bridge_failed`, `bridge_cost_recorded` all written with work_item_id

### Seam 4->5 Tests (Phase 5 implementer verifies these)

- [ ] Quality probe against bridge provider tests through cloud_relay path (not bypass to local GPU)
- [ ] Clawback works for bridge job (credits clawed back, not dollars — bridge operator's dollar cost is their problem)
- [ ] Bridge provider quality hold suspends bridge offers (offer status changes, no new matches)
- [ ] Different anomaly detection thresholds for `provider_type='bridge'` vs `provider_type='local'`
- [ ] Challenge staking: filing a challenge locks stake proportional to `actual_cost`
- [ ] Rejected challenge: challenger's stake forfeited to challenged provider
- [ ] Upheld challenge: provider's credits clawed back, challenger bounty paid, DADBEAR breaker hold placed
- [ ] No zero-cost challenges (DD-9: economic gates)
- [ ] Challenge evidence protocol respects privacy (Wire never sees payloads even during challenge)

### Seam 5->6 Tests (Phase 6 implementer verifies these)

- [ ] Quality hold blocks Phase 6 work item dispatch (DADBEAR holds respected)
- [ ] Hold clear emits observation event -> Phase 6 compiler triggers fresh observation cycle
- [ ] Fresh observation cycle completes before steward resumes experiments on previously-held model
- [ ] Reputation data feeds into steward provider selection (low-reputation providers ranked lower)
- [ ] Phase 6 observation sources (heartbeat demand, chronicle health, network config) produce DADBEAR observation events
- [ ] Compiler mappings produce work items (model_portfolio_eval, pricing_adjustment, market_depth_adjustment)
- [ ] Result application: Ollama load/unload works, pricing contribution supersession works, experiment publication works
- [ ] Management LLM calls (sentinel 2b model) can access GPU without starving market jobs (queue bypass or dedicated slot resolved)
