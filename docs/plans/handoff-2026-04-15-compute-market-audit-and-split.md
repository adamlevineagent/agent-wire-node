# Session Handoff: Compute Market Plan Audit & Split

**Date:** 2026-04-15
**Scope:** Re-audited Phases 2-9 of the compute market build plan against the post-DADBEAR/Chronicle/Fleet codebase. Split the 2,150-line monolith into 7 per-phase docs. Ran two audit rounds (informed + discovery). Identified corrections needed before implementation.

---

## What Shipped This Session

### Round 1: Informed Audit (8 agents, 2 per phase group)
- 17 unique critical, 28 unique major findings
- Central finding: plan predates DADBEAR, Chronicle, and Fleet — market jobs have no path through work items, holds, or the supervisor
- Phases 6-9 collapse into one phase (DADBEAR already provides the architecture)
- Consolidated findings: `audit-2026-04-15-compute-market-phases-2-9.md`

### Plan Split: 7 New Documents
All in `agent-wire-node/docs/plans/`:

| File | Lines | Content |
|------|-------|---------|
| `compute-market-architecture.md` | 1,608 | Cross-cutting reference: principles, schemas, RPCs, DADBEAR integration, chronicle events, economic parameters |
| `compute-market-phase-2-exchange.md` | 660 | Exchange & Matching |
| `compute-market-phase-3-settlement.md` | 657 | Settlement & Requester Integration |
| `compute-market-phase-4-bridge.md` | 679 | Bridge Operations |
| `compute-market-phase-5-quality.md` | 1,195 | Quality & Challenges |
| `compute-market-phase-6-intelligence.md` | 752 | Market Intelligence via DADBEAR (collapsed 6-9) |
| `compute-market-seams.md` | 626 | Inter-phase integration, handoff contracts, failure cascades |

The original monolith (`wire-compute-market-build-plan.md`) is now historical reference only.

### Round 2: Discovery Audit (7 agents, 1 per doc)
- 16 critical, 22 major, 21 minor across all 7 docs
- No architectural problems found — DADBEAR integration sound, Phase 6 collapse confirmed valid
- Findings are cross-doc contradictions, doc-to-deployed divergence, and mechanism gaps

---

## What Needs Fixing (Prioritized)

### P0: Design Hole — Result Content Delivery Path (Phase 3)

The one genuine design question this session surfaced but did not resolve.

**The problem:** In the Phase 3 settlement flow, the provider sends only metadata to the Wire (`/compute/settle` with token counts, latency, finish_reason). The actual LLM output — the result content — has no described path from provider to requester.

**Why it's hard:** The privacy model says the Wire never sees payloads. But the provider doesn't know the requester's tunnel URL (privacy). Two options:

1. **Wire-proxied result delivery (standard tier):** Wire receives result content from provider, forwards to requester webhook. Wire sees the payload. This is consistent with 0-relay launch privacy (Wire already sees the prompt for standard tier). Simple. But must be explicit that Wire transiently handles result content for standard tier.

2. **Provider-direct result delivery:** The fill RPC returns a `result_delivery_url` (the requester's tunnel URL, possibly via relay chain). Provider POSTs result directly. Wire never sees content. But this gives the provider the requester's tunnel URL, which the privacy model specifically prohibits for standard tier.

**Decision needed:** Which path for launch? Recommendation: option 1 (Wire-proxied) for standard tier at launch, option 2 for relay tiers later. This must be reflected in the Phase 3 doc and the architecture doc's privacy model.

### P1: Cross-Doc Contradictions (fix in one pass)

These are naming/ownership conflicts from parallel agent writing. Each has a clear resolution — just need someone to pick the canonical name and grep-fix all docs.

| Contradiction | Where | Resolution |
|--------------|-------|------------|
| Phase 5 table names | Seams says `wire_compute_challenges`, Phase 5 says `wire_compute_challenge_cases` | **Phase 5 doc is canonical** — it has the full schema. Fix seams doc. |
| Phase 5 RPC names | Seams invents `aggregate_compute_reputation` | **Doesn't exist.** Remove from seams. The real RPCs are in Phase 5 doc. |
| Observation aggregation ownership | Phase 3 handoff claims it, Phase 5 workstream claims it, seams assigns to both | **Phase 3 builds it, Phase 5 consumes it.** Clarify in all three. The function ships in Phase 3 because Phase 5 depends on populated data. |
| `cancel_compute_job` ownership | Phase 2 doc defers to Phase 3, seams assigns to Phase 2 | **Phase 3.** Cancel requires deposit refund logic that builds on Phase 3's settlement patterns. Fix seams. |
| `settle_compute_job` return type | Phase 3 describes void, architecture returns TABLE | **Architecture doc is canonical** — it has the full SQL. Fix Phase 3 to reference the return type. |

### P2: Doc-to-Deployed Divergence (mark, don't fix)

The architecture doc's RPCs describe the *corrected/target* state. The deployed migrations (`20260414100000`, `20260414200000`) have the *pre-audit* state. This is correct — the docs are build plans for what to build, not descriptions of what's deployed. But it needs explicit marking.

**What to do:** Add a section to the architecture doc: "Deployed vs Target State." For each RPC, mark whether it's deployed (with pre-audit signature) or new (Phase N). Specific items:

| RPC | Deployed? | Differences from doc |
|-----|-----------|---------------------|
| `match_compute_job` | Yes | Missing self-dealing check, missing reputation filter |
| `fill_compute_job` | Yes | No `relay_count` param, no `requester_operator_id` param, different return type |
| `settle_compute_job` | Yes | Has duplicate operator resolution, missing `model_id` filter on queue decrement |
| `fail_compute_job` | Yes | Missing `model_id` filter |
| `void_compute_job` | Yes | Missing `model_id` filter |
| `start_compute_job` | No | New in Phase 3 |
| `cancel_compute_job` | No | New in Phase 3 |
| `clawback_compute_job` | No | New in Phase 5 |
| `aggregate_compute_observations` | No | New in Phase 3 |

Also: economic parameter names in deployed seed contributions differ from doc names. The deployed names are canonical — update the architecture doc to match deployed names: `default_output_estimate` (not `default_output_estimate_tokens`), `staleness_thresholds` (not `stale_offer_threshold_minutes`), seconds not minutes.

### P3: Mechanism Gaps (amend docs)

Each needs a sentence or paragraph added to the relevant doc:

| Gap | Doc | Fix |
|-----|-----|-----|
| DADBEAR preview gate may reject already-paid market jobs | Phase 2 | Specify: market work items enter as `"previewed"` (skip preview) or preview for compute-market slug is a no-op pass-through. The Wire's deposit IS the cost gate — DADBEAR preview is redundant for provider-side market jobs. |
| Programmatic OpenRouter key provisioning doesn't exist | Phase 4 | Remove option 1 (POST /api/v1/keys). Manual key is the only path. Update BridgeConfigPanel to require manual key entry. |
| `debit_operator_atomic` has no partial-debit mode | Phase 5 | Clawback partial path must use raw SQL (direct UPDATE + INSERT to ledger), not the atomic RPC. Same pattern as `settle_compute_job`'s platform subsidy path. |
| `wire_challenge_bank.source_type` CHECK constraint | Phase 5 | Add a prerequisite migration: `ALTER TABLE wire_challenge_bank DROP CONSTRAINT ...; ALTER TABLE wire_challenge_bank ADD CONSTRAINT ... CHECK (source_type IN (..., 'compute_probe'))` |
| `deactivate_stale_compute_offers` would overwrite quality holds | Phase 5 | Add `AND status NOT IN ('quality_hold', 'timing_suspended', 'reputation_suspended')` to the staleness sweep WHERE clause |
| DADBEAR preview is USD-only, market needs credits | Phase 3 | Preview gate for market:compute slug uses credits (not USD). Add a currency field to `ItemCostEstimate` or a parallel `estimated_cost_credits` field. |
| `map_event_to_primitive` signature needs change | Phase 6 | Signature changes from `(event_type: &str)` to `(event: &ObservationEvent)`. Note this affects `dadbear_compiler.rs:362` call site. |
| `apply_result` has no market dispatch surface | Phase 6 | Acknowledge this is new construction — 5 new match arms in `apply_result`, each calling different subsystems (Ollama IPC, contribution supersession, Wire API). |
| `clawed_back` status invisible to existing queries | Phase 5 | Add `AND status != 'clawed_back'` to observation aggregation and reputation queries. |
| 403 retry behavior conflict for bridge | Phase 4 | Bridge dispatch handler must override default `retryable_status_codes` to exclude 403. Note the existing default at `llm.rs:347`. |
| FleetPeer has no `provider_types` field | Phase 4 | Migration note: existing fleet peers without the field should default to `["local"]`, not empty (which would skip them entirely). |

### P4: Pillar 37 Violations (minor, fix during implementation)

| Violation | Location |
|-----------|----------|
| Exponential backoff constants (1s, 2s, 4s, 30s) | Phase 2 queue mirror |
| DADBEAR supervisor constants (5s tick, 300s SLA, 300s preview TTL, 30d retention) | `dadbear_supervisor.rs` |
| `advance_market_rotator` doc version hardcodes `% 80` | Architecture doc (deployed version is correct — reads from contribution) |

---

## What Does NOT Need Fixing

The discovery audit confirmed these are correct:

- Phase 6 collapse (4→1) is architecturally valid. Nothing from original phases is lost.
- DADBEAR hold system IS extensible (any string hold type works without code changes).
- Per-model queue separation IS correct (management model gets its own queue automatically).
- Existing `parse_openai_shaped_response` already extracts `actual_cost_usd` (Phase 4 can reuse).
- ACK pattern comment already exists in `server.rs:1051` (Phase 3 has a starting point).
- The rotator arm economics are correct (76/2/2 from contribution, not hardcoded in deployed code).

---

## File Index

All in `agent-wire-node/docs/plans/`:

```
wire-compute-market-build-plan.md          -- HISTORICAL: original monolith (2026-04-13)
audit-2026-04-15-compute-market-phases-2-9.md  -- Round 1 audit findings
compute-market-architecture.md             -- ACTIVE: cross-cutting reference
compute-market-phase-2-exchange.md         -- ACTIVE: Phase 2 implementation guide
compute-market-phase-3-settlement.md       -- ACTIVE: Phase 3 implementation guide
compute-market-phase-4-bridge.md           -- ACTIVE: Phase 4 implementation guide
compute-market-phase-5-quality.md          -- ACTIVE: Phase 5 implementation guide
compute-market-phase-6-intelligence.md     -- ACTIVE: Phase 6 implementation guide (collapsed 6-9)
compute-market-seams.md                    -- ACTIVE: inter-phase integration guide
handoff-2026-04-15-compute-market-audit-and-split.md  -- THIS FILE
handoff-2026-04-15-compute-market-session.md  -- Phase 1 build session handoff
```

---

## Suggested Work Order

1. **P0: Decide result delivery path** — 5 min decision, then update Phase 3 + architecture docs
2. **P1: Fix cross-doc contradictions** — grep + replace, 5 items, 30 min
3. **P2: Add deployed vs target markers** — architecture doc amendment, 20 min
4. **P3: Amend mechanism gaps** — 11 items, each a sentence or paragraph, ~1 hour total
5. **P4: Note Pillar 37 violations** — implementers can fix inline, just flag them

After these corrections, the docs should pass a clean audit.

---

## Session Stats

- 8 informed auditors (Round 1) + 7 discovery auditors (Round 2) = 15 total audit agents
- 7 doc-writing agents
- 1 codebase survey agent
- Round 1 raw findings: 23 critical, 43 major, 27 minor (before dedup)
- Round 2 raw findings: 16 critical, 22 major, 21 minor
- Documents produced: 8 (7 plan docs + 1 audit record)
- Total new content: ~6,200 lines
