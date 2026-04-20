# Resumption brief — Compute Market Phase 3

**Written:** 2026-04-20 pre-compact
**Purpose:** This file is the single anchor for picking up Phase 3 work after conversation compaction or a fresh session. Read this first; it points you at every file you need and every decision already made.

---

## TL;DR — one-paragraph state

Phase 3 = the provider-delivery hop of the compute market. Both sides have spec-only work complete but no rev-2.0 code yet. **Node-side spec is at rev 0.6.1** (commit `69c2c77`); the prior rev-0.5 implementation is already shipped as commits `5faff2d` + `46bd4cd` + `974d37a` but it targets the **wrong architecture** (Wire-in-middle). Rev 2.0 contract landed on Wire as `838b7700` with a P2P delivery reversal — provider POSTs content directly to requester, settlement metadata separately to Wire. Wire-side build plan is at `9e5da9c8` + cross-audit fixes at `f2b35ba0`. **Both sides have a spec that matches the contract and each other. Implementation has not started on rev 0.6.1 on either side.** The next action is either another audit pass or starting the 20-step rev-0.6.1 implementation.

---

## Files to read first (in this order)

1. **This file.**
2. **Node spec rev 0.6.1** — `docs/plans/compute-market-phase-3-provider-delivery-spec.md`. THE definitive spec for what to implement on the node side. 500+ lines. Contains state machine, schema, envelope adapters, chronicle taxonomy, build order (20 steps), tests (44+), audit history across 6 revs.
3. **Wire contract rev 2.0** — `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md`. Pinned at commit `838b7700`. Particularly §2.1 (dispatch body with new `requester_callback_url` + `requester_delivery_jwt` fields), §2.3 (settlement — Wire's callback endpoint, repurposed for metadata-only), §2.6 (P2P content delivery direct provider→requester), §3.4 (requester-delivery JWT claim shape), Q-PROTO-4/6.
4. **Wire build plan rev 2.0** — `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/plans/compute-market-phase-3-wire-side-build-plan-rev-2.0.md`. Latest commit `f2b35ba0` (cross-audit fixes). Defines Wire's R1/R2/R3 ship order + workstreams α-ε + WS6-9 reshape.
5. **Bilateral decisions doc** — `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/architecture/compute-market-p2p-decisions-2026-04-20.md` (referenced by the contract rev 2.0 commit). Captures D1-D8 and Q-PROTO-4/6 resolutions.
6. **P2P reversal plan** (context for why rev 0.6 replaced rev 0.5) — `/Users/adamlevine/AI Project Files/GoodNewsEveryone/docs/plans/compute-market-phase-3-p2p-reversal-2026-04-20.md`.
7. **Canonical privacy arch** — `docs/canonical/63-relays-and-privacy.md` (node repo). Source of truth for Wire-is-coordinator-not-content-carrier framing.

Historical / can skip unless investigating:
- `docs/plans/compute-market-phase-3-wire-owner-handoff-2026-04-20.md` — my paste-back to Wire owner after rev 0.4 audit (pre-P2P-reversal; outdated architecture but decision trail is useful).
- `docs/plans/compute-market-staleness-handoff-2026-04-20.md` — unrelated, from the staleness-filter incident earlier same day.

---

## Key decisions already made — do NOT re-open these

Every line here represents a bilateral agreement. If you find yourself questioning any of these, read the associated decision source before raising, because the same question has likely been resolved already.

### Architectural

- **Two-POST topology (Q-PROTO-4).** Provider POSTs content to requester (§2.6), settlement to Wire (§2.3). Wire is zero-storage for content (§2.4). P2P is the ONLY Phase 3 topology; pre-rev-2.0 `compute_callback_mode` economic_parameter is deprecated.
- **Per-leg retry budget (Q-PROTO-6 / D8).** Content leg + settlement leg have independent `max_attempts` budgets from `compute_delivery_policy` economic_parameter (default 5 each). Shared `backoff_schedule_secs = [1, 5, 30, 300, 3600]`.
- **Concurrency = single cap across legs** (bilateral Q1 2026-04-20). `for_each_concurrent(max_concurrent_deliveries)` over flat `(row_id, leg)` pairs. Cap bounds outbound HTTP/socket budget as a shared resource. NOT per-leg.
- **Content body size = implementation-recommended 10 MiB bounded reader** (Q2 2026-04-20). Not a protocol cap (Pillar 37 — Wire doesn't prescribe LLM output size). Requester-side 413 on overflow.
- **Legacy aud: clean-cut, no dual-aud transition.** Node spec previously had a "transition window" accepting both `aud="result-delivery"` (legacy) and `aud="requester-delivery"` (new). Removed in rev 0.6.1 — contract §3.4 sanctions only the new aud; legacy tokens self-expire in ≤`fill_job_ttl_secs`; any in-flight legacy fails 401 → content leg exhausts → requester reconciles.
- **Failure envelope flows to BOTH legs (D4).** Worker failure (model_timeout, oom, worker_heartbeat_lost, etc.) produces `{type: "failure", job_id, error: {code, message}}` on both the content POST (so requester stops polling) and the settlement POST (so Wire has reputation-scoring metadata).
- **Requester-offline fallback = (a)+(b) per D5, NOT dead-letter on Wire.** Provider retries content leg until `max_attempts_content` exhausts; content is then lost; requester reconciles via its own local `pending_jobs` map (oneshot never fires → waiter times out). **UPDATE 2026-04-20 in Wire's f2b35ba0:** `delivery_attested_at` column dropped from contract §2.4 entirely (no mechanism to populate). Wire's visibility ends at settlement; requester reconciliation is local-only, NOT Wire-poll-based. ⚠️ Node spec rev 0.6.1 still says "requester polls Wire to reconcile" in 2-3 places — **this is a known open item; see "Open items" below.**
- **Privacy: Phase 3 is attributed (§7.1).** Provider sees requester's tunnel URL on content POST. Relay layer fixes this in a future phase. Accept for Phase 3.

### Contract shapes (node MUST consume these exactly)

- **Dispatch body fields** (Wire → provider, §2.1): adds `requester_callback_url: string` + `requester_delivery_jwt: string` to the existing §2.1 body. `callback_url` + `callback_auth` field NAMES unchanged; their SEMANTIC repurposes to settlement-only.
- **Requester-delivery JWT** (§3.4): `aud="requester-delivery"` (distinct from "compute" and legacy "result-delivery"), `iss="wire"`, `sub=<uuid_job_id>` (NOT handle-path — §10.5), `rid=<requester_operator_id>`, EdDSA, same `WIRE_DOCUMENT_JWT_PRIVATE_KEY`. Provider is opaque (stores + echoes); requester verifies via new `verify_requester_delivery_token`.
- **Settlement envelope** (§2.3, node → Wire): §2.3 shape MINUS `result.content`. Strict rejection 400 `settlement_carried_content` if content present. Same `error.code` enum (`worker_heartbeat_lost | model_timeout | oom | invalid_messages | model_error`).
- **Content envelope** (§2.6, node → requester): §2.3 full shape WITH `result.content`. `body.job_id` is UUID (§10.5 + Pillar J7). Failure variant is identical shape to settlement-failure — `{type: "failure", ...}`.
- **X-Wire-Retry header** (`never | transient | backoff`): emitted by Wire on non-2xx of settlement endpoint. Node reads on settlement leg only. NOT applicable to content leg (arbitrary requester responses don't standardize it).
- **`compute_delivery_policy`** economic_parameter (new in rev 2.0): `{max_attempts_content: 5, max_attempts_settlement: 5, backoff_schedule_secs: [1, 5, 30, 300, 3600]}`. Seeded by Wire's migration in R1 (per cross-audit fix). Node reads via heartbeat `wire_parameters` allow-list.
- **`requester_delivery_jwt_ttl_secs`** economic_parameter (new): default = `fill_job_ttl_secs` = 1800s.
- **`privacy_tier`** Q-PROTO-3: string, warn-don't-reject on unknowns. Phase 3 value = `"direct"` only. Deprecated `"bootstrap-relay"` still accepted (logged, treated as direct).

### Chronicle naming

- Node emits: `market_result_delivered` (both legs 2xx), `market_content_leg_succeeded`, `market_content_leg_failed`, `market_settlement_leg_succeeded`, `market_settlement_leg_failed`, `market_result_delivery_failed` (both terminal), `market_delivery_task_panicked`/`_exited`, `market_wire_parameters_updated`, `market_unknown_privacy_tier`.
- Rev-0.5 names (`market_result_delivered_to_wire`, `_cas_lost`, etc.) kept in local SQLite for back-compat on historical rows but **DO NOT EMIT in rev 0.6**.
- Wire-side grandfathers `compute_result_delivered` + `compute_result_forwarded_to_requester` in its CHECK constraint but MUST NOT emit them in rev-2.0 code.
- `delivery_status` terminal values on the node's outbox match Wire's `wire_compute_jobs.delivery_status` enum: `awaiting_settlement | settled | failed_content_only | failed_settlement_only | failed_both | expired_unsettled | no_callback`. (Rev 0.6.1 renamed node's local "failed" → "failed_both" to align.)

---

## What's shipped on each side (code, not plans)

### Node side — commits on `main`, BUT these are rev 0.5 and target the WRONG architecture

- `5faff2d` — Phase 3 schema + types plumbing. 5 new columns on `fleet_result_outbox` (`callback_auth_token`, `delivery_lease_until`, `delivery_next_attempt_at`, `inference_latency_ms`, `request_id`). `OutboxRow` struct extension. `pyramid_schema_versions` table. `AuthState.wire_parameters` field. `MarketDispatchContext.delivery_nudge` field.
- `46bd4cd` — Delivery worker + integrations. `src-tauri/src/pyramid/market_delivery.rs` (780 lines). Supervisor + claim CAS + lease + POST + envelope adapter. `spawn_market_worker` failure-branch fix (still correct in rev 2.0). Heartbeat `wire_parameters` self-heal. Main.rs spawn wiring.
- `974d37a` — DeliveryHealth frontend indicator.
- `69c2c77` — Rev 0.6.1 spec (this file's authoritative plan).

**The rev-0.5 code is dead letter.** It POSTs to Wire's callback URL expecting Wire to forward content. Rev 2.0 says provider POSTs content directly to requester. When implementation starts, most of the module's structure survives (supervisor, retry logic, envelope serialization primitives) but URLs + Bearers + the single-POST model get surgically rewritten. Per rev 0.6.1 estimate: ~60% of rev 0.5 code survives; 8-16h rework.

### Wire side

- `838b7700` — Contract rev 2.0 (source of truth).
- `9e5da9c8` — Build plan rev 2.0.
- `f2b35ba0` — Cross-audit fixes (6 MAJOR + 3 MINOR from the 3-agent audit run this session). Also dropped `delivery_attested_at` from contract §2.4.

Wire has shipped no rev-2.0 code yet either. Both sides are plan-complete, implementation-pending.

---

## Open items (must handle before/during implementation)

1. **Node spec references to "requester polls Wire for delivery_attested_at"** — 2-3 places in rev 0.6.1 still say "requester reconciles via §2.4 status-poll" or similar. Wire owner's f2b35ba0 dropped `delivery_attested_at` and updated D5 to "requester reconciles locally against its own `pending_jobs` map." Exact lines where the node spec needs updating:
   - Line ~43: "requester polls `/api/v1/compute/jobs/:job_id` and sees `delivery_status = failed_content_only`"
   - Line ~76: "Requester polls Wire to reconcile"
   - Line ~239: "requester reconciles via §2.4 status-poll"
   - Line ~515: "requester reconciles via §2.4 status-poll"

   These should change to something like: "requester reconciles via local `pending_jobs` timeout — if the oneshot never fires within `max_wait_ms`, requester knows content never arrived; no Wire-poll involved for content-delivery attestation." Do this as rev 0.6.2 patch commit before implementation.

2. **Whether to run one more cross-audit on rev 0.6.1 + Wire's f2b35ba0.** Wire owner says "your rev 0.6 spec is unaffected [by f2b35ba0] — ready to implement on both sides unless you find something in this patch round." I didn't find a blocker; only the status-poll wording drift above. Depending on Adam's call: (a) patch the spec to 0.6.2 inline and skip another full audit, (b) run another 3-agent cross-audit as insurance, (c) go straight to implementation.

3. **Implementation start trigger** — unknown who hits GO. The rev 0.6.1 build order has 20 steps; roughly:
   1. Migration + columns + OUTBOX_SELECT update
   2. `market_outbox_insert_or_ignore` signature + callers
   3. `MarketDispatchRequest` struct additions
   4. `handle_market_dispatch` admission validation
   5. Split envelope adapters (`build_content_envelope`, `build_settlement_envelope`)
   6. Per-leg DB helpers (claim, mark_posted_ok, bump_attempt, mark_failed)
   7. Rewrite `tick()` + `deliver_one` → `deliver_leg(leg)`
   8. Chronicle event constants
   9. `ComputeDeliveryPolicy` node struct + heartbeat parse
   10. `verify_requester_delivery_token` in `result_delivery_identity.rs`
   11. `handle_compute_job_result` handler switch
   12. Frontend `DeliveryHealth` updates
   13. 6 new tests (+ existing tests reshape)
   14. `cargo check` + `cargo test`
   15. Commit + push
   16. Wait for Wire to ship rev 2.0 code (or match pace)
   17. Rebuild Playful + BEHEM
   18. Smoke test
   19. Serial verifier (Pillar 39)
   20. Done

4. **Serial verifier / post-ship audit** — after both sides ship rev-2.0 code, a fresh-eyes agent checks for drift between implementation and spec. Not optional per Pillar 39 / `feedback_serial_verifier`.

---

## Resumption questions (what future-me needs to answer)

Read these in order. Each has a pointer to where the answer lives — you should be able to answer each in <5 min by reading the pointed-to file.

**Phase 1 — reconstruct state:**

1. **What's the most recent commit on agent-wire-node `main`?** — Run `git log --oneline -5` in the repo. Expect `69c2c77` as top (or later if Adam's pushed rev 0.6.2 patches since this brief).
2. **What's the most recent commit on GoodNewsEveryone `main`?** — Same in that repo. Expect `f2b35ba0` as top of P3 plan branch unless newer.
3. **Is rev-2.0 code shipped on either side?** — grep `GoodNewsEveryone/src/app/api/v1/compute/` for `requester_delivery_jwt` or `requester_callback_url`. If grep returns non-test matches with recent commit dates, Wire has started implementing. For node: grep `agent-wire-node/src-tauri/src/pyramid/market_delivery.rs` for the same. If either side has started coding, the brief's "plan-complete, implementation-pending" framing may be outdated.

**Phase 2 — reconcile spec drift:**

4. **Does the node spec rev 0.6.1 still say "requester polls Wire to reconcile" anywhere?** — `grep -n "polls Wire\|status-poll\|delivery_attested_at" docs/plans/compute-market-phase-3-provider-delivery-spec.md`. Should be zero matches post-rev-0.6.2 patch. If there are matches, apply the fix in Open Item #1.
5. **Does Wire's contract rev 2.0 still reference `delivery_attested_at`?** — `grep -n "delivery_attested_at" /Users/adamlevine/AI\ Project\ Files/GoodNewsEveryone/docs/architecture/wire-node-compute-market-contract.md`. Should be zero post-f2b35ba0.
6. **Does Wire's build plan have the 6 cross-audit MAJOR fixes applied?** — grep the plan for: `P0411`, `compute_callback_mode retraction`, `CHECK constraint ALTER` in R1, `delivery_attested_at` dropped (should not appear). If all present/absent-as-expected, cross-audit is resolved.

**Phase 3 — decide next action:**

7. **Does Adam want another audit cycle on rev 0.6.1 + f2b35ba0?** — Check the most recent messages for his GO/WAIT/ANOTHER-AUDIT call. If unclear, he's probably waiting on the rev 0.6.2 wording fix (Open Item #1) to land before calling it.
8. **Should I start implementation?** — Only if Adam has explicitly said GO. Default: don't start without direction.
9. **If starting implementation, what's step 1?** — Read `docs/plans/compute-market-phase-3-provider-delivery-spec.md` §"Build order" step 1 (verify contract — already done, §838b7700) and step 2 (migration + columns). Begin with step 2.

**Phase 4 — test-run context:**

10. **Does the current dev build on upstairs (Playful) have rev-0.5 running?** — `ps aux | grep wire-node-desktop` + `lsof -i :8765`. If process PID from around 2026-04-18 is still running, it's stale rev-0.5 code; any smoke will exercise the wrong architecture. Either kill + rebuild after rev-2.0 ships, or leave it running on the old code until implementation lands.
11. **Is BEHEM downstairs running rev-0.5 too?** — Ask Adam (I can't reach BEHEM directly). BEHEM needs a rebuild when rev-2.0 ships.
12. **What's the smoke command?** — Same as before: `curl -s -m 120 -X POST http://localhost:8765/pyramid/compute/market-call -H "Authorization: Bearer test" -H "Content-Type: application/json" -d '{"model_id":"gemma4:26b","prompt":"Say hello in one word","max_budget":100000,"max_tokens":20,"max_wait_ms":60000}'`. But this will NOT work correctly until rev-2.0 is on both sides — rev-0.5 code expects Wire to forward content, which Wire no longer does.

**Phase 5 — background context:**

13. **What's the overall goal?** — GPU-less tester installs the app, asks a question, their pyramid builds using foreign GPUs, they never see a market word. This is THE test. Implementation phase ships the last piece (provider delivery); without it, Wire never hears back from providers and no pyramid ever completes on a GPU-less node.
14. **Where's the purpose brief I should re-read if I forget?** — `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_compute_market_purpose_brief.md`. May be stale re: specific commits/state but the purpose framing is load-bearing.
15. **Who owns Wire side?** — Adam relays paste-backs. The Wire-side agent is separate. Messages to Wire owner should be self-contained (they have no memory of this conversation).

---

## Memory entries worth updating after compact

These are the auto-memory files (`~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/`) that may need refreshing once rev-2.0 ships and the purpose brief's current state changes:

- `project_compute_market_purpose_brief.md` — last updated 2026-04-17; describes rev 0.5-era state (Wire-in-middle). Needs update to "P2P delivery, rev 2.0 contract, rev 0.6.1 spec" framing.
- `project_compute_market_phase_2_shipped.md` — might need a sibling entry `project_compute_market_phase_3_rev2_state.md` capturing where rev-2.0 work stands once it ships.

Don't rewrite these pre-implementation; do it once rev-2.0 code is live and smokes through.

---

## First action on resumption

1. Read this file (you're doing it).
2. Answer resumption questions 1-6 to reconstruct state.
3. If state matches this brief's expectations (rev 0.6.1 spec + f2b35ba0 Wire plan, no implementation yet), apply the rev-0.6.2 patch from Open Item #1.
4. Ask Adam for GO before starting the 20-step implementation.

If state has advanced (implementation started, smoke ran, etc.), skip to whatever the most recent chat message indicates as the next blocker.
