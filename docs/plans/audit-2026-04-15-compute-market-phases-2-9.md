# Compute Market Phases 2-9 — Consolidated Audit Findings

**Date:** 2026-04-15
**Scope:** Re-audit of Phases 2-9 against current codebase (post-DADBEAR, post-Chronicle, post-Fleet)
**Method:** 8 independent auditors (2 per phase group), findings deduplicated and merged
**Plan under audit:** `wire-compute-market-build-plan.md` (written 2026-04-13, last audited same day)

---

## Executive Summary

The plan was written before three major systems shipped: DADBEAR canonical architecture, Compute Chronicle, and Fleet Dispatch. These systems fundamentally change how Phases 2-9 should be structured. The plan describes building infrastructure that now exists (Phases 6-9), references schemas that have diverged (Phases 2-3), and assumes challenge infrastructure that is structurally incompatible with compute disputes (Phase 5).

**Raw findings:** 23 critical, 43 major, 27 minor across 8 auditors.
**After dedup:** 17 unique critical, 28 unique major.

The findings cluster into 7 cross-cutting themes. The plan needs amendment before implementation.

---

## Theme 1: DADBEAR Integration Gap (ALL phases)

DADBEAR shipped after the plan was written. The handoff doc explicitly states "market jobs should flow through DADBEAR work item system." The plan doesn't mention DADBEAR anywhere.

### What DADBEAR provides
- **Compiler:** observe → compile → preview → dispatch → apply
- **Work items:** durable, semantic path IDs, CAS state transitions, crash recovery
- **Holds:** frozen/breaker/cost_limit, append-only hold events with projection
- **Supervisor:** 5-second reconciliation tick, in-flight recovery on restart
- **Preview gate:** cost/routing/policy evaluation before dispatch commitment

### Per-phase impact

| Phase | Gap | Severity |
|-------|-----|----------|
| 2-3 | Market jobs (requester + provider side) bypass work items, holds, preview, crash recovery. Requester crashes mid-job → paid but no result, no recovery. | Critical |
| 4 | Bridge jobs don't create DADBEAR work items | Major |
| 5 | DADBEAR breaker holds not connected to Wire market participation. Challenged provider keeps serving during dispute. | Major |
| 6 | "Daemon intelligence" IS the DADBEAR compiler processing market observation events — plan builds it from scratch | Critical |
| 7 | "Sentinel" IS a DADBEAR observation source — plan builds a separate reconciliation loop | Critical |
| 8 | "Steward experiment loop" IS the DADBEAR observe→compile→preview→dispatch→apply lifecycle | Critical |
| 9 | "Steward chains" ARE action chains processed by the existing chain executor | Major |

### Recommendation
- Phases 2-3: Market jobs flow through DADBEAR work items. Provider side creates work items on job receipt (`source: "market_received"`). Requester side creates work items for outbound market calls, going through preview gate (cost estimation via market pricing).
- Phase 5: Upheld challenge places a quality hold on Wire offers + DADBEAR breaker hold locally via heartbeat propagation.
- Phases 6-9: Collapse into one phase. Three DADBEAR slugs (`market:compute`, `market:storage`, `market:relay`) replace three daemons. New observation sources, compiler mappings, and result application paths — not new systems.

---

## Theme 2: Chronicle Integration Gap (Phases 2-5)

The Compute Chronicle shipped with stubs for `market`/`market_received` event sources but no call sites exist.

| Phase | Missing Chronicle Events |
|-------|------------------------|
| 2 | `market_offered`, `market_matched`, `market_received` |
| 3 | `market_fill`, `market_dispatched`, `market_settled`, `market_failed`, `market_voided` |
| 4 | `bridge_dispatched`, `bridge_returned`, `bridge_failed`, `bridge_cost_recorded` |
| 5 | Chronicle as challenge evidence (requester-side timing vs provider-reported timing) |

Each event must carry `work_item_id` and `attempt_id` for DADBEAR correlation. The chronicle columns already exist.

---

## Theme 3: Relay Privacy Model Underspecified (Phases 2-4)

### 3a: Phase 3 data flow contradicts relay-first architecture [Critical]
The `WireComputeProvider.fill_job()` code sketch sends `system_prompt, user_prompt` as parameters. But the actual `fill_compute_job` RPC accepts only `p_input_token_count` and `p_relay_count` — NOT prompts. The Wire never sees payloads. The plan will mislead implementers into sending prompts to the Wire.

**Fix:** Rewrite Phase 3 post-fill flow: (1) fill RPC returns relay_chain + provider_ephemeral_pubkey, (2) requester encrypts prompt, (3) sends through relay chain to provider, (4) awaits result back through relay chain.

### 3b: `select_relay_chain` function referenced but never defined [Major]
Called in `fill_compute_job` but no `CREATE FUNCTION` exists. Either stub it (reject relay_count > 0 until relay market ships) or list relay market as a cross-plan dependency.

### 3c: 0-relay market jobs need explicit specification [Major]
For launch, market jobs with `relay_count=0` should use direct Wire-proxied dispatch (Wire sees payloads — acceptable for standard tier). This matches current OpenRouter privacy level. Plan should state this explicitly.

### 3d: Bridge privacy degradation undisclosed [Critical]
Bridge offers show `privacy_capabilities: '{standard}'` — same as local GPU. But prompts flow through bridge node + OpenRouter + upstream provider. Requesters can't tell.

**Fix:** Bridge offers must carry `'cloud_relay'` privacy indicator. Dispatch policy must allow requester-side filtering ("never route to bridge providers").

---

## Theme 4: SQL Bugs in Plan RPCs (Phases 2-3)

| Bug | RPCs Affected | Severity |
|-----|--------------|----------|
| Missing `AND model_id = v_job.model_id` in queue state UPDATE | `settle_compute_job`, `fail_compute_job`, `void_compute_job` | Critical |
| `fill_compute_job` references `v_wire_platform_operator_id` but never declares or resolves it | `fill_compute_job` | Critical |
| `settle_compute_job` resolves Wire platform operator twice (duplicate query) | `settle_compute_job` | Major |
| `fill_compute_job` calls `select_relay_chain` twice (non-deterministic relay selection) | `fill_compute_job` | Minor |
| No `filled → executing` status transition defined | All settlement RPCs check for `status='executing'` but nothing produces it | Major |
| No `cancel_compute_job` RPC defined (S11 says "must be added" but isn't) | N/A | Major |
| `QueueEntry` schema in plan completely diverged from code (5 missing fields, different structure) | Phase 2 node workstream | Critical |

---

## Theme 5: Quality Enforcement Gap (Phase 5)

Phase 5 claims "quality enforcement" but delivers only "quality dispute infrastructure." Both auditors converge on this.

### 5a: Challenge infrastructure structurally incompatible [Critical]
Existing system is a Tier 1 answer-key bank for entity extraction (`wire_challenge_bank`). Tier 2 challenge panels are a draft design doc with no migrations. Phase 5 depends on infrastructure that doesn't exist.

### 5b: No clawback RPC [Critical]
Provider was paid at settlement. To claw back: need to debit provider (who may have spent the credits), handle negative balance, determine Graph Fund treatment, fund challenger bounty. No `clawback_compute_job` exists.

### 5c: No observation aggregation function [Critical]
Phase 5 depends on aggregated performance data (reputation signals). The `wire_compute_offers.observed_*` columns exist but nothing populates them. Phase 3 punts: "run on schedule or in settlement."

### 5d: Privacy vs evidence tension [Major]
Wire never sees payloads (by design). But challenge panels need evidence to adjudicate disputes. Plan doesn't reconcile these. Must choose: (a) requester opts into revealing prompt for challenge, (b) challenges limited to timing/metadata anomalies, or (c) hash-then-re-run protocol.

### 5e: No proactive detection until Phase 8 [Critical]
Lazy provider (cached responses + artificial delay) is undetectable by timing analysis or reactive challenges. Steward comparison testing listed in Phase 5 but steward doesn't exist until Phase 8. Suggest: extend existing honeypot challenge infrastructure to compute (Wire dispatches known-answer test jobs at random intervals).

### 5f: No challenge staking — Sybil attack viable [Critical]
DD-9 says "economic gates, not rate limits" but Phase 5 doesn't apply this to challenges. No cost to filing. If 1/3 of false challenges are erroneously upheld, challenge spam is profitable.

**Fix:** Challenge stake proportional to job `actual_cost`. Rejected challenge forfeits stake to challenged provider.

### 5g: No self-dealing check [Major]
`match_compute_job` doesn't verify `requester_operator_id != provider_operator_id`. Self-dealing for reputation inflation is standard marketplace attack.

### 5h: Timing anomaly detection entirely unspecified [Major]
One sentence in the plan, no algorithm, no trigger, no threshold source, no action on detection.

---

## Theme 6: Bridge Double-Settlement Surface (Phase 4)

Phase 4 is an 18-line sketch for what is fundamentally a dual-currency settlement system.

### 6a: No dollar↔credit conversion mechanism [Critical]
Bridge operator receives credits, pays OpenRouter dollars. No mechanism reads current OpenRouter cost-per-token and derives a credit floor. "Pure market pricing" works for local GPU (stable electricity cost) but not for real-time dollar API costs.

### 6b: Double-settlement failure modes [Critical]
Wire settlement succeeds + OpenRouter billing fails = operator profits. Wire settlement fails + OpenRouter succeeds = operator loses money. No atomic-or-compensating protocol.

### 6c: Rate limit isolation [Critical]
Personal builds and bridge jobs share one OpenRouter API key. Bridge traffic exhausts server-side rate limits, blocking personal builds. Settlement layer has programmatic key provisioning — suggest bridge-dedicated key.

### 6d: Error code mapping [Critical]
OpenRouter 429/503/402/400 each need different Wire job state transitions. No mapping specified. A 402 (insufficient funds) should suspend ALL bridge offers immediately.

### 6e: Model lifecycle [Critical]
Auto-detected models deprecated on OpenRouter while Wire offer stays active. No refresh interval, no diff-and-deactivate sweep.

### 6f: Fleet vs bridge dispatch ordering [Major]
Same-operator fleet routing could route own builds through bridge (paying OpenRouter dollars for own inference). Fleet dispatch must prefer local GPU over bridge.

### 6g: Cloudflare 120s timeout [Major]
Bridge adds relay hops + OpenRouter latency. ACK+async (handoff TODO) not in Phase 4 scope but bridge makes it critical.

### 6h: `compute_bridge` contribution schema never defined [Major]
Listed in contribution types table but no YAML example, no fields, no defaults.

---

## Theme 7: Phases 6-9 Collapse

Both auditors independently concluded the same thing: four phases building four systems collapse into one phase extending DADBEAR with market-domain specifics.

### What's actually new work
1. **3 observation sources:** heartbeat demand signal extractor, chronicle health monitor, network config fetcher
2. **Compiler mappings:** `demand_signal → model_portfolio_eval`, `throughput_drift → pricing_adjustment`, `queue_utilization → market_depth_adjustment`
3. **Result application paths:** call Ollama control plane (load/unload/swap), supersede pricing contributions, publish experiment results
4. **Market chain YAML:** steward decision chains (existing executor, new step types)
5. **Wire-side:** publication contribution type, config recommendation RPC, subscription mechanism
6. **Frontend:** market intelligence dashboard, model portfolio view, pricing optimizer, experiment log

### What does NOT need to be built
- No new reconciliation loop (DADBEAR supervisor)
- No new event tables (DADBEAR observation/hold events)
- No new hold system (DADBEAR holds)
- No new work item lifecycle (DADBEAR work items)
- No new crash recovery (DADBEAR supervisor)
- No new chain executor (existing one)
- No separate daemon/sentinel/steward processes (DADBEAR compiler + observation sources)
- No three daemon instances (three DADBEAR slugs: `market:compute`, `market:storage`, `market:relay`)

### Implementation feasibility concerns (from Auditor B)
1. **GPU access for sentinel/steward LLM calls:** Compute queue is sole serializer. Sentinel 2b model call sits behind market jobs. Need management-class queue bypass or dedicated small-model VRAM reservation.
2. **Model loading state machine:** Loading takes minutes. No specification for when offers/queues appear relative to loading state. Need: `deciding → downloading → loading_vram → warming_up → ready` with offer lifecycle tied to state.
3. **Cross-market VRAM conflict:** Unified memory is zero-sum. Serial experiments across compute/storage can't discover joint optima. Need resource budget allocation before individual daemon experiments.
4. **Experiment statistical validity:** No minimum sample size. Low-traffic nodes thrash on zero-data experiments. Need minimum job count + degraded-mode falling back to network configs.
5. **Publication privacy:** Steward analysis leaks competitive intelligence (pricing strategy, revenue figures, hardware profile). Need field-level publishable/private distinction.

---

## Pillar Violations Found

| Pillar | Violation | Location |
|--------|-----------|----------|
| 37 | 500-token output estimation fallback | `match_compute_job`, `fill_compute_job` |
| 37 | 5-minute stale offer threshold hardcoded | `deactivate_stale_compute_offers` |
| 37 | DADBEAR supervisor constants hardcoded (5s tick, 300s SLA, etc.) | `dadbear_supervisor.rs` |
| 9 | `QueueDiscountPoint.multiplier` is `f64` not integer basis points | Plan line 1263 |
| 9 | `ComputeJob.matched_multiplier` is `f64` | Plan line 1278 |
| 9 | `ComputeOffer.rate_per_m_input` is `u64` not `i64` (per J14 correction) | Plan line 1253 |
| 18 | Bridge jobs go through OpenRouter HTTP API, different path than local GPU | Phase 4 |

---

## Recommended Plan Amendments

### Before implementing Phase 2
1. Rewrite `QueueEntry` spec against actual code schema (5 new fields)
2. Specify DADBEAR work item integration for both requester and provider sides
3. Fix all SQL bugs (model_id filter, undeclared variable, duplicate resolution, status transitions)
4. Define 0-relay market flow explicitly (Wire-proxied for standard tier at launch)
5. Stub `select_relay_chain` to reject relay_count > 0
6. Specify queue mirror recovery (reconnect push, debounce window, seq conflict)
7. Replace 500-token fallback with `economic_parameter` contribution
8. Add chronicle write points for all market events

### Before implementing Phase 3
1. Rewrite `WireComputeProvider` to NOT send prompts to Wire
2. Specify post-fill relay chain data flow
3. Add `cancel_compute_job` RPC
4. Define `filled → executing` transition
5. Specify ACK+async result delivery (prerequisite, not optional)
6. Integrate with DADBEAR preview gate for cost estimation

### Before implementing Phase 4
1. Specify dollar↔credit conversion mechanism (read OpenRouter cost, derive floor)
2. Design error classification table (OpenRouter HTTP → Wire job state + offer state)
3. Require bridge-dedicated OpenRouter API key
4. Add `cloud_relay` privacy indicator on bridge offers
5. Specify model refresh lifecycle (periodic check, diff-and-deactivate)
6. Define fleet dispatch ordering (local GPU > bridge for same-operator)
7. Define `compute_bridge` contribution schema
8. Add bridge-specific chronicle events

### Before implementing Phase 5
1. Build or spec Tier 2 challenge panel infrastructure (currently a draft doc with no migrations)
2. Design `clawback_compute_job` RPC with negative balance handling
3. Implement observation aggregation function (Phase 3 dependency)
4. Design compute challenge evidence protocol (privacy vs evidence)
5. Add challenge staking (DD-9 economic gates pattern)
6. Add `requester_operator_id != provider_operator_id` check in matching
7. Design proactive quality probes (extend honeypot infrastructure to compute)
8. Specify timing anomaly detection algorithm, trigger, and threshold source

### Before implementing Phases 6-9
1. Rewrite as single phase: "Market Intelligence via DADBEAR"
2. Define new observation event types for market domain
3. Define compiler primitive mappings for market decisions
4. Define result application paths (Ollama control plane, contribution supersession)
5. Specify GPU access for management LLM calls (queue bypass or dedicated model)
6. Define model loading state machine with offer lifecycle
7. Specify experiment minimum sample size and degraded-mode
8. Design publication privacy (field-level publishable/private)

---

## Auditor Agreement Matrix

Findings where both auditors in a pair independently identified the same issue (highest confidence):

| Finding | Auditor A | Auditor B |
|---------|-----------|-----------|
| DADBEAR absent from Phases 2-3 | C2 | C3 (Finding 3) |
| model_id filter missing in 3 RPCs | M6 | Finding 1 |
| Duplicate operator resolution in settlement | M5 | Finding 8 |
| Phase 3 data flow contradicts privacy model | — | C4 (Finding 4) — Auditor A caught relay boundary but not the prompt-to-Wire contradiction |
| Challenge infrastructure incompatible | C1 (5A) | — (5B focused on game theory, not infrastructure) |
| No proactive quality detection | — (5A noted steward gap) | C1 (lazy provider attack) |
| DADBEAR makes Phases 6-9 redundant | C1-C3 (collapse assessment) | M1 (redundancy finding) |
| Bridge DADBEAR absent | M4 (4A) | M4 (4B) |
| Bridge Chronicle absent | — | M4 (4B) |
| Cloudflare timeout critical for bridge | M3 (4A) | M3 (4B) |
