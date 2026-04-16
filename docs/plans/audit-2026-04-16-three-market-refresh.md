# Audit 2026-04-16: Three-Market Refresh Pass

**Scope:** The 10 plan docs refreshed in the 2026-04-16 session — compute-market-architecture, compute-market-phase-{2..6}, compute-market-seams, storage-market-conversion-plan, relay-market-plan, fleet-mps-build-plan.

**Method:** Two-stage blind audit. Two informed auditors (split by scope: A=compute internals, B=cross-market+storage+relay). Two discovery auditors (C=fresh read, D=implementer wanderer "can I build Phase 2 tonight?"). All four ran against the plans plus shipped codebase (fleet.rs, tunnel_url.rs, fleet_identity.rs, fleet_delivery_policy.rs, local_mode.rs, compute_queue.rs, deployed migrations). No pyramid-knowledge — direct reads.

**Raw counts:** A=8C/12M · B=4C/10M · C=7C/14M · D=5 blockers/10 friction. Roughly 24 critical + 46 major before dedup. Many cross-auditor agreements; deduped below.

---

## Verdict

**Not ready to implement as written.** The doc set has a few root-cause issues that each spawn multiple surface findings. Fixing the root causes (below) collapses most of the critical and major findings in a single pass. Estimated docs-only work to bring this to implementation-ready: one focused pass (1–2 hours if serialized; less with targeted agents on disjoint files).

Phase 4 and Phase 6 are the most buildable. Phase 5 is close. **Phase 2 and Phase 3 need a unification pass** — they carry contradictory dispatch flows side-by-side.

The architectural design is sound: the SOTA privacy model, the ACK+callback+outbox transport, the rotator-arm settlement, the DADBEAR-everywhere discipline, the participation-policy-as-single-operator-surface. What's broken is delivery quality — successive edit passes accumulated without a consolidation sweep, and several claims about "shipped" foundations don't match the actual codebase state.

---

## Critical Findings (correctness blockers — must fix before implementation)

### CR-1. DADBEAR slug namespace is inconsistent across 7 docs → Phase 5 quality holds can't block Phase 2 work items

Same logical slug appears as `compute-market`, `market:compute`, `compute-market-bridge`, and `storage-market` / `market:storage` across the doc set. Phase 5 §VI places holds on `"market:compute"`. Phase 2 §V creates work items under `"compute-market"`. A DADBEAR hold filter checks slug string literal — different strings means **the hold never fires**. This is a correctness blocker, not a cosmetic one.

**Flagged by:** A-C7, C-C7/M13/T1, B (implicit via seams §VIII reference). Triple cross-auditor agreement.

**Offending sites:**
- Phase 2 §V lines 556, 574; §III line 388: `"compute-market"`
- Phase 3 §III lines 523, 557: `"compute-market"`
- Phase 4 §E line 387, §F line 428: `"compute-market-bridge"` (bridge as separate slug)
- Phase 5 §VI lines 508, 512, 538, 1178: `"market:compute"`
- Phase 6 §II line 52+: `"market:compute" / "market:storage" / "market:relay"`
- Seams §VIII line 717: `"compute-market" / "storage-market" / "relay-market"`

**Fix:** Pick one. Recommend `market:compute / market:storage / market:relay` (Phase 6's convention — groups cleanly, `<namespace>:<market>` reads right). Grep all 10 docs. Replace all variants. Add a "DADBEAR Slug Namespace" locked statement to `compute-market-architecture.md` §VI. Decide whether bridge is a separate slug (for hold independence from local-GPU quality) or a `step_name` suffix within `market:compute` — current docs inconsistent on this.

---

### CR-2. Phase 2 and Phase 3 still carry pre-SOTA "Wire-proxied standard tier" framing that contradicts architecture §III

Thread B1 rewrote architecture §III to the SOTA model (three orthogonal mechanisms, Wire-as-bootstrap-relay as transient). Phase 2 §I/§VII and Phase 3 §III still describe "Wire-proxied dispatch for standard tier" as the launch stance, and Phase 3 §III has the old flow ("Wire proxies prompt to provider") and the new flow (ACK+callback with callback_url in envelope) literally side-by-side in the same doc, in different subsections. An implementer picks whichever they read first.

Phase 3's old flow also references a `POST /api/v1/compute/submit-prompt` endpoint that appears nowhere in the architecture §X API routes table and nowhere in the actual dispatch protocol — it was invented in the old `WireComputeProvider` code sketch.

**Flagged by:** A-C1, A-C2, C-C1, C-C2. Quadruple cross-auditor agreement.

**Offending sites:**
- Phase 2 §I line 30: "Privacy model for this phase: 0-relay market jobs use Wire-proxied dispatch"
- Phase 2 §VIII audit corrections (lines 702, 704): same language
- Phase 3 §III lines 253–308: old "submit-prompt" + "Wire proxies the prompt" flow
- Phase 3 §III lines 381–434: new ACK+callback flow — mutually exclusive with 253–308
- Phase 3 §VIII line 667–668: still labels launch tier "Wire-proxied dispatch for standard tier"

**Fix:** In Phase 2 §I, replace the privacy paragraph with a pointer to architecture §III (bootstrap mode is the launch reality; the protocol shape is SOTA-from-day-one). In Phase 3 §III, delete lines 253–308 entirely (the old `WireComputeProvider.call()` code sketch with `submit-prompt`). The canonical flow is already in §III 381–434 — keep only that. Also delete the `/api/v1/compute/submit-prompt` endpoint from any API table references.

---

### CR-3. `clawback_compute_job` has two incompatible RPC signatures

Architecture §IX and Phase 5 §III define the same RPC with different signatures, different parameters, different return types, different side effects. Seams doc agrees with Phase 5. Architecture is stale.

**Flagged by:** A-C3, C-C4. Double cross-auditor agreement.

**Divergence:**
- Architecture §IX (line 1262): `clawback_compute_job(p_job_id, p_challenger_operator_id, p_challenge_stake) RETURNS void`. Bounty = `v_clawback_amount / 10`. Refunds `v_job.actual_cost`.
- Phase 5 §III (line 133): `clawback_compute_job(p_job_id, p_verdict_id) RETURNS TABLE(provider_debited, challenger_credited, negative_balance_claim)`. Reads challenger + stake from `wire_compute_challenge_cases` via `p_verdict_id`. Inserts into `wire_compute_negative_claims` on partial debit. Sets offer status to `'quality_hold'`.

Phase 5's version is correct — it is internally consistent with `file_compute_challenge` and `resolve_compute_challenge`, which call `clawback` from inside `resolve` and have only `case_id` available at that point.

**Fix:** Update architecture §IX to Phase 5's signature. Add a §VIII.5 row for `clawback_compute_job` noting the signature is Phase 5-owned. Delete the architecture §IX body version.

---

### CR-4. `aggregate_compute_observations` has three incompatible signatures

**Flagged by:** C-C5.

- Phase 3 §II: `(p_node_id, p_model_id, p_horizon_hours DEFAULT 168) RETURNS TABLE(median_latency_ms, p95_latency_ms, median_tps, median_output_tokens, observation_count, failure_count)` — per-node/per-model read helper for the heartbeat
- Phase 5 §VIII: `() RETURNS INTEGER` — no-arg sweep function that writes `observed_*` columns across all active offers
- Architecture §IX: `(p_node_id, p_model_id) RETURNS void` — no horizon parameter

**Fix:** These are genuinely two different functions. Rename one. Recommend `aggregate_compute_observations_for(node_id, model_id, horizon)` for the read helper and `refresh_offer_observations_sweep()` for the sweeper. Document both in seams doc. Delete the architecture §IX stub as redundant with Phase 5's sweeper.

---

### CR-5. `compute_participation_policy.allow_market_dispatch` is referenced as shipped but does not exist in the deployed struct

Seams §VIII line 733 and memory index claim "Fleet MPS WS1+WS2 shipped (commit 4ae01c0)." But the deployed `ComputeParticipationPolicy` struct at `local_mode.rs:1719-1727` has 5 fields, missing `allow_market_dispatch`. The bundled contribution at `bundled_contributions.json:218` also omits it. Phase 3 §III's dispatch-chain gate reads a field that doesn't exist today. Phase 2's offer-publication gate has the same problem for `allow_market_visibility` which DOES exist — but the 4 storage/relay booleans (`allow_storage_hosting`, `allow_storage_pulling`, `allow_relay_serving`, `allow_relay_usage`) are ALSO missing and referenced by storage/relay overlays.

**Flagged by:** A-M1, B-C2, D-B5. Triple cross-auditor agreement.

**Fix:** Phase 2 scope must include: (a) extend `ComputeParticipationPolicy` in `local_mode.rs` to add the 5 missing fields; (b) supersede the bundled contribution JSON; (c) add a `mode`→booleans projection function (currently `mode` and the booleans are independent fields with no projection logic, despite plan claiming mode projects to the booleans). Also: correct seams §VIII to say "partial WS1+WS2 shipped — 5-field struct + contribution scaffold; remaining 5 fields land with Phase 2." Don't assert foundation is complete when it isn't.

---

### CR-6. `CallbackKind` enum variants in the docs don't match the shipped enum

Architecture §III (lines 94–101) and seams §VIII use variants `RequesterTunnel / RelayChain / WireBootstrap / FleetPeer`. The shipped enum at `fleet.rs:582-588` has 3 variants: `Fleet { dispatcher_nid } / MarketStandard / Relay` — the last two marked `KindNotImplemented` reserved for market Phase 3. The docs renamed the enum without anyone renaming the code, and `MarketStandard`/`Relay` have different semantics (unit variants) than the new names (which thread `callback_url` through the envelope as a separate field).

**Flagged by:** A-M3, B-C4, D-F1. Triple cross-auditor agreement.

**Fix:** Pick a convention. Either (a) rename the shipped enum to match docs (breaking change to the one fleet call site, manageable), or (b) keep the shipped names (`Fleet / MarketStandard / Relay`) and fix all the docs back to those names. Either is fine; the docs and code must converge before the Phase 2 handler ships. Add `validate_callback_url` semantics per variant — right now "accept any HTTPS because the callback is Wire-signed in the JWT" is implicit but not stated.

---

### CR-7. `MarketDispatchRequest.messages: serde_json::Value` diverges from deployed `FleetDispatchRequest.{system_prompt, user_prompt}` — "field-for-field reuse" claim is false

Phase 2 §III line 397 defines `MarketDispatchRequest.messages: Value` (ChatML array) and claims "Matches FleetDispatchRequest field-for-field except where noted." Deployed `FleetDispatchRequest` at `fleet.rs:258-273` has `system_prompt: String` and `user_prompt: String` — different shape on the most important field. No conversion helper exists; downstream `QueueEntry` has `system_prompt`+`user_prompt` strings.

**Flagged by:** A-C8, D-B2. Double cross-auditor agreement.

**Fix:** Pick one:
1. Retrofit fleet dispatch to `messages: Value`. Breaking change, adds scope to Phase 2.
2. Keep fleet as two strings; make `MarketDispatchRequest` use the same shape. Defer the J16 fix to a future phase.
3. Document explicitly in Phase 2 §III that market diverges from fleet on prompt shape and spell out a `messages_to_prompt_pair(messages) -> (String, String)` canonical helper (collapse policy: last system message, concatenate user messages, reject conversations with assistant turns for Phase 2).

Option 3 is minimal viable. Option 1 is cleanest but expensive. Option 2 defeats one of the plan's stated audit corrections.

---

### CR-8. `fill_compute_job` RPC deployed signature doesn't accept `p_relay_count` but Phase 2 API route sends it and §IX shows expanded signature

Architecture §IX shows `fill_compute_job(p_job_id, p_requester_operator_id, p_input_token_count, p_relay_count)` with rejection for `p_relay_count > 0`. Deployed migration (20260414200000:587) has `fill_compute_job(p_job_id, p_input_token_estimate, p_temperature, p_max_tokens)`. §VIII.5 line 687 correctly flags the divergence and says "Phase 2 migration extends signature" — but Phase 2 §II migration list (lines 167–172) does NOT include a fill-extension migration. Phase 2 §II line 97 API body includes `relay_count` and §VI line 628 verification curl sends it. Route sends a param the RPC can't accept.

**Flagged by:** A-C4, D-F6. Double cross-auditor agreement.

**Fix:** Phase 2 §II migration list must include: `ALTER FUNCTION fill_compute_job` (or DROP + CREATE) adding `p_relay_count INTEGER DEFAULT 0` (reject >0), and `p_requester_operator_id UUID` (for caller identity). Also: API body should pass `temperature` and `max_tokens` that §VI verification currently omits — otherwise the deployed `settle_compute_job`'s `max_tokens * 2` completion-token guard never fires (see CR-10).

---

### CR-9. `wire_compute_jobs.status` and `wire_compute_offers.status` have no CHECK constraint, but Phase 5 references "CHECK expansion"

Deployed migration (20260414200000:45, :90) has `status TEXT` with only enumeration comments — no CHECK constraint. Phase 5 §III adds `'clawed_back'` status and Phase 5 §VI adds `'quality_hold' / 'timing_suspended' / 'reputation_suspended'`. Phase 5 §XVI line 1238 says "`wire_compute_offers.status` CHECK expansion" — there is no CHECK to expand. An implementer writing a CHECK based on this text will either pin status to the initial set (breaking Phase 5) or invent a set from the text (guess work).

**Flagged by:** A-C5.

**Fix:** Phase 5 migration list must clarify: there is no existing CHECK; Phase 5 adds one covering all present values including the new quality/timing/reputation statuses. Or explicitly: rely on RPC-level validation and don't add a CHECK. Pick one and make it explicit. If CHECK is added, the CHECK must include both jobs.status and offers.status value sets.

---

### CR-10. `select_relay_chain` is simultaneously four things — stub, dead code, full impl, inline rejection

- Architecture §IX: `fill_compute_job` has `IF p_relay_count > 0 THEN RAISE EXCEPTION` and never calls `select_relay_chain`
- Phase 2 §II line 57: "fill_compute_job MUST reject p_relay_count > 0"
- Phase 2 §II: calls `select_relay_chain` "twice non-deterministically — dead code in Phase 2"
- Seams §III Phase 2 migration list: "select_relay_chain stub — returns empty result set"
- Relay plan §V: full implementation

**Flagged by:** C-C3, A-U5. Double agreement.

**Fix:** Decide whether `select_relay_chain` exists as a function at compute market launch. Recommended: no separate function, rejection lives inline in `fill_compute_job` at Phase 2. The relay market plan creates the function when it ships. Update seams doc to remove "stub" from Phase 2 migration list. Purge "dead code" language from Phase 2 §II.

---

### CR-11. Phase 5 `resolve_compute_challenge` requires `status = 'paneling'` but nothing transitions `open → paneling`

`file_compute_challenge` inserts with default status `'open'`. Status transitions to `'resolved'` happen in `resolve_compute_challenge`. But the `open → paneling` transition is unspecified — no RPC, no workflow. `resolve_compute_challenge` will always raise 'Case not found or not in paneling status.'

**Flagged by:** C-C7.

**Fix:** Either add a `select_adjudication_panel(p_case_id)` RPC to Phase 5 §IX that transitions `open → paneling` after panelists chosen, OR broaden `resolve_compute_challenge`'s status filter to `IN ('open', 'paneling')`. The panel selection mechanism described in §II also lacks an implementation handle — pick one and spec it.

---

### CR-12. `wire_hosting_grants.min_replicas INTEGER NOT NULL DEFAULT 2` reintroduces the Pillar 37 violation the plan claims to be fixing

Storage plan §V line 455 declares `min_replicas INTEGER NOT NULL DEFAULT 2`. §I item 9 / §II.9 / §IX all say `min_replicas` must be contribution-driven. The DDL default 2 either defeats the contribution or is dead (grant creator must always specify).

**Flagged by:** B-C3.

**Fix:** Drop `NOT NULL DEFAULT 2`. Make the column nullable (fall back to `economic_parameter` contribution at match/payout time) OR required-with-no-default (caller always supplies from policy). The §III grant-creation YAML example (line 217) already shows `min_replicas: 3`, confirming the grant creator supplies it.

---

### CR-13. Neither `ServiceDescriptor` nor `AvailabilitySnapshot` exists in code, but Phase 2 §III consumes them as if they do

Phase 2 §III lines 175–194 describes `ServiceDescriptor.{models_loaded, servable_rules, visibility, protocol_version}` and `AvailabilitySnapshot.{total_queue_depth, health_status, tunnel_status, degraded}` as shipped fleet-MPS objects that Phase 2 "consumes." Grep confirms neither exists: `fleet-mps-three-objects.md:5` explicitly states "Plan approved, not yet implemented." Seams §VIII says WS1+WS2 shipped — what actually shipped is the `compute_participation_policy` contribution; the three-objects runtime model did not.

**Flagged by:** A-M2, D (codebase clarification #4).

**Fix:** Add an explicit prerequisite to Phase 2 §I: "Fleet MPS WS1 + WS2 three-objects shipped (ServiceDescriptor + AvailabilitySnapshot structs land in code)." Correct seams §VIII to list only "partial Fleet MPS: compute_participation_policy contribution" as shipped. Update the `project_async_fleet_shipped.md` memory similarly. Move the three-objects build into Phase 2 scope OR land it in a separate focused pass before Phase 2.

---

### CR-14. `compute_result_outbox` table schema is "implementer's call" — unbuildable

Seams §VIII lists `compute_result_outbox` as an introduced table. Phase 2 §III claims "inherit outbox scaffolding." But no doc specifies: (a) whether to share `fleet_result_outbox` or create a parallel table, (b) the PK scheme for the market case where "dispatcher" is the Wire (not a fleet peer), (c) the full CAS helper set (fleet has ~14 functions in `db.rs:2332-2655`), (d) whether market jobs skip the `delivered` state that fleet uses.

Storage plan overlay claims its own outbox (`storage_result_outbox`) but the body never declares it.

**Flagged by:** B-M3, D-B3. Double agreement.

**Fix:** Phase 2 §III must either (a) decide share-or-split on the outbox table and spec the PK scheme + CAS helper set, or (b) explicitly reuse `fleet_result_outbox` with a new `CallbackKind` variant that carries market metadata. Storage plan S1 must declare `storage_result_outbox` DDL + sweep loop, or drop the outbox claim from its overlay.

---

### CR-15. `MarketIdentity` verifier's correctness check is undefined

Phase 2 §III says "same shape as `FleetIdentity`, different aud claim." But `FleetIdentity` checks `claims.op == self_operator_id` — a same-operator invariant that DOES NOT hold for market dispatch (requester ≠ provider operator). Docs don't specify:
- The canonical check for a market JWT (issuer-pinned to Wire signing key? Provider node_id match? Job_id-lookup gate?)
- Claim field names (`op`, `nid`, `sub`, `rid`, `pid`?)
- Whether the public key source is the same as the fleet JWT public key

Without this, step 1 of `handle_market_dispatch` cannot be written.

**Flagged by:** D-B1.

**Fix:** Architecture §III or Phase 2 §III must specify full `MarketClaims` shape (claim fields), verifier check surface (sig, exp, provider_nid match, issuer-pin), and key source. This is also a Wire-side spec gap — the Wire's `POST /api/v1/compute/fill` handler must issue a JWT, with a defined shape, with a defined signing key. D-F7 covers the issuance side.

---

## Major Findings

### MJ-1. Phase 3 § outbox delivery worker is owed to Phase 3 but unspecified

Phase 2 §III worker path says "Phase 3 dispatch loop delivers the result to `callback_url` and settles." Phase 3 §III §ACK+Async section describes Wire-side result-relay but NOT the provider-side outbox delivery loop that reads `status='ready'` rows, POSTs to callback_url, transitions to `status='delivered'`. Scaffolding gap. [A-M4]

**Fix:** Add "Outbox Delivery Worker" subsection to Phase 3 §III: provider-side loop reading outbox rows with `status='ready'`, POSTing to callback_url, handling backoff per `market_delivery_policy`, transitioning to `delivered` on 2xx.

### MJ-2. `market_delivery_policy` contribution referenced everywhere but never defined

Phase 3 §II claims the contribution exists with unspecified field list. No seed YAML path. No DB singleton table naming. No registration path in `config_contributions`. Compared to the well-specified `fleet_delivery_policy` (18 fields, hot reload, seed YAML, DB helpers), `market_delivery_policy` is a handwave. [D-B4]

**Fix:** Spec the full field list (likely parallel to `fleet_delivery_policy`'s 18 fields with market-specific adjustments), seed YAML at `docs/seeds/market_delivery_policy.yaml`, DB singleton table, `config_contributions::sync_config_to_operational_with_registry` mapping entry, and Rust `Default` impl with bootstrap sentinels. Phase 2 ships it; Phase 3 consumes it.

### MJ-3. `start_compute_job` RPC has no caller specified on the node side

Seams §II T3 + Phase 2 §VII both say `start_compute_job` ships in Phase 2. Who calls it? The GPU loop when it dequeues? The handler after `enqueue_market`? DADBEAR supervisor on `dispatched` transition? Without a caller, the RPC ships orphaned and Phase 3 settlement (which requires `status='executing'`) always fails. [D-F5]

**Fix:** Phase 2 §III must specify: GPU loop calls `POST /api/v1/compute/start` (which invokes `start_compute_job`) immediately before dequeuing a market job and before calling the LLM. Name the function in the GPU loop that does this.

### MJ-4. Architecture §IX `advance_market_rotator` inline SQL still shows `% 80` despite disclaimer comment

The body still says `DO UPDATE SET position = (wire_market_rotator.position % 80) + 1`. The disclaimer says "deployed is canonical, do not copy this." An implementer cargo-culting will reintroduce the Pillar 37 violation. [A-M5]

**Fix:** Replace the architecture §IX inline SQL with the deployed canonical version that reads `total_slots` from the contribution. Disclaimer alone is insufficient.

### MJ-5. Architecture §IX RPCs use `h.status = 'active'` but deployed uses `h.released_at IS NULL`

Different predicate. A handle with `status='suspended'` but not released matches deployed but not arch. §VIII.5 RPC deployment table does not flag this divergence. [A-C6]

**Fix:** Architecture §IX should use deployed predicate `h.released_at IS NULL`, OR §VIII.5 should add a row explicitly documenting the divergence. Pick one and reconcile.

### MJ-6. Phase 3 `cancel_compute_job` relay-fee refund branch is a `NULL;` no-op

When relay market ships, `cancel_compute_job` with `relay_count > 0` will silently fail to refund relay fees. Classic silent deferral — violates `feedback_no_deferral_creep`. [A-M12]

**Fix:** Either (a) raise an exception in the relay branch so it can't silently fail when relay ships, OR (b) implement the ledger sum + credit_operator_atomic before shipping Phase 3. The relay plan should note the dependency and the refactor timing.

### MJ-7. Participation policy field list divergence across 5 docs

The canonical set of `compute_participation_policy` fields differs between:
- fleet-mps-build-plan §Canonical Runtime Model: 6 fields
- compute-market-phase-2-exchange §III: adds semantic clarifications on 6
- storage-market-conversion-plan overlay §Foundation 2: adds 2 storage fields
- relay-market-plan overlay §Foundation 3: adds 2 relay fields
- compute-market-seams §VIII: lists all 10

WS1 bundled schema_definition built against fleet-mps's 6-field list will reject storage/relay supersessions. [B-C1, C-M6, B-M10]

**Fix:** Fleet-mps-build-plan.md must enumerate all 10 fields as canonical, even those consumed by later phases. Add Gate A requirement that WS1 bundled schema_definition includes all 10. Pick one doc as single source of truth (recommend fleet-mps-build-plan) and have the other four reference it.

### MJ-8. Relay overlay contradicts itself on `relay_delivery_policy` vs `market_delivery_policy`

Relay overlay Foundation 1 (line 26) says relay uses `relay_delivery_policy`. Line 74 says "R1 adds: use `market_delivery_policy`." Line 75 says `relay_delivery_policy` again. Seams §VIII line 682 lists four distinct contributions. [B-M1]

**Fix:** Pick `relay_delivery_policy` (matches seams parallel-per-market pattern). Correct overlay line 74.

### MJ-9. Hardcoded "heartbeat fresh" / `interval '2 minutes'` constants in storage and relay bodies (Pillar 37)

Storage §VII line 673 "heartbeat fresh". Relay §V line 505 `AND n.last_seen_at > now() - interval '2 minutes'`. Both should read from deployed `staleness_thresholds.heartbeat_staleness_s` economic_parameter contribution. [B-M2]

**Fix:** Replace hardcoded intervals with contribution reads. Pattern already exists in deployed `deactivate_stale_compute_offers` — copy it.

### MJ-10. Relay body `TunnelRotationState` still uses `String` despite overlay Foundation 4 fix

Overlay Foundation 4 lines 52–64 gives the struct as `TunnelUrl`. Body §VI.3 lines 587–595 still shows `String`. Implementer reading body copies `String`. [B-M5]

**Fix:** 2-line edit in body §VI.3. Replace `String` with `TunnelUrl`.

### MJ-11. Storage plan §V schema lacks outbox table declaration despite overlay claim

Storage overlay claims "uses the same outbox pattern as compute: provider can be offline briefly and catch up when connectivity returns." Storage body has zero occurrences of "outbox." No DDL, no lifecycle, no sweep loop. [B-M3]

**Fix:** Either add to Storage S1 Wire-workstream: `storage_result_outbox` table + sweep loop, OR drop the outbox claim from the overlay (if storage settlements are idempotent enough to not need it).

### MJ-12. Storage overlay claims async pattern AND synchronous streaming for pulls — mutually exclusive

Storage overlay says "Provider streams the body back synchronously (or delivers asynchronously for chunked assets)" AND "uses the same outbox pattern as compute." Sync streaming hits the same Cloudflare 524 the async pattern was built to solve for large assets. [B-M4]

**Fix:** Pick one. Recommended: pulls ≤ N MB (seeded) are synchronous streams; larger assets use chunked-asset manifest with per-chunk async tokens. Drop "outbox pattern" claim if synchronous is the launch mode for all pulls. Document the size threshold.

### MJ-13. Build ordering graph over-serializes Storage S1 behind Compute Phase 2

Seams §VIII graph: `Compute Phase 2 → Storage S1`. Storage plan §0 prereq: only Compute Phase 1. Storage S1's needs (rotator, `wire_graph_fund` CHECK, atomic RPCs) are all Phase 1. None require Phase 2 matching/queue/self-dealing. [B-M7]

**Fix:** Seams §VIII graph: Storage S1 depends on Compute Phase 1 only. S1 can develop concurrently with Compute Phase 2 and Phase 3. Relay R1 still depends on Phase 2 (for `select_relay_chain` inline rejection) + S1 (for settlement pattern).

### MJ-14. Phase 4 `FleetPeer.provider_types` field addition doesn't extend announce/heartbeat transport

Phase 4 §V.D adds `provider_types: Vec<String>` to `FleetPeer`. Fleet routing in `llm.rs` then splits into local-fleet and bridge-fleet passes based on the field. But the `HeartbeatFleetEntry` type (fleet.rs:83–106) and the fleet announce payload are NOT extended. Field exists on the struct but never gets populated from the wire. [A-M10]

**Fix:** Phase 4 §V.D add concrete items: extend `HeartbeatFleetEntry` with `provider_types`; extend fleet announce endpoint body; name the `llm.rs` fleet dispatch function being modified; specify interaction with existing `serving_rules`.

### MJ-15. `AppState` + `ServerState` wiring for market dispatch unspecified — ~10 edit points missing

`AppState` has no `compute_market_state` field. `ServerState` has `fleet_dispatch` (from async-fleet-dispatch) but no `market_dispatch`. Mirror push background task has no owner. GPU loop handle-to-outbox not plumbed. [D-F3]

**Fix:** Phase 2 §III Boot Ordering subsection (new) enumerating: fields to add to AppState, fields to add to ServerState, background tasks to spawn in `main.rs::setup`, Arc bundle composition for `MarketDispatchContext` (parallel to `FleetDispatchContext`), and the interaction between the GPU loop's `oneshot::Sender` and the outbox CAS writer.

### MJ-16. DADBEAR preview short-circuit for market slug has no reference implementation

Phase 2 §V "P3 fix — preview gate is a no-op for provider-side market jobs" says either enter at `'previewed'` directly, or pass-through in `dadbear_preview.rs`. Neither path has a reference implementation or spec for what fields (`preview_id`, `batch_id`, `epoch_id`, `primitive`) get set on a direct-at-previewed insert. [D-F2]

**Fix:** Specify the bypass-path INSERT fully: which fields are required, which are nullable in this state, whether `preview_id` is set to a synthetic "no-preview" sentinel or NULL. Or specify the `dadbear_preview.rs` short-circuit branch concretely (which functions check the slug, what they return).

### MJ-17. Queue mirror push loop ownership and channel plumbing unspecified

Phase 2 §III "Queue Mirror" describes a `tokio::sync::mpsc` channel but doesn't specify sender-side call sites (~10+ across `enqueue_local`, `enqueue_market`, `dequeue_next`, fleet dispatch admission, DADBEAR supervisor, llm.rs compute-queue interceptor), receiver-side task ownership, debounce contribution read path, or seq management on node re-registration. [D-F4]

**Fix:** Spec the mpsc sender injection pattern (probably `Arc<mpsc::Sender>` on the queue manager), the receiver task's startup location (`main.rs::spawn`), and the seq persistence/reconciliation flow on node reboot.

### MJ-18. `wire_job_token` JWT issuance on the Wire side is entirely unspecified

Provider's `handle_market_dispatch` verifies `wire_job_token`. Who issues? When? With what claims? Which signing key? The Wire's `/api/v1/compute/fill` handler dispatches to the provider with a JWT — nothing in Phase 2 Wire workstream describes the token's shape or signing path. [D-F7]

**Fix:** Architecture §IX or Phase 2 Wire workstream must spec: token claims shape (matches CR-15 market identity spec), signing key source (shared fleet key? dedicated market key?), TTL policy (`fleet_jwt_ttl_secs` parameter — reuse or new).

### MJ-19. `/api/v1/compute/submit-prompt` endpoint referenced in Phase 3 code but exists nowhere else

Phase 3 §III old `WireComputeProvider.call()` code sketch references `POST /api/v1/compute/submit-prompt`. Architecture §X API routes table does not list it. Nothing else in the doc set references it. It's a ghost endpoint from a pre-SOTA draft. [Subsumed by CR-2.]

**Fix:** Delete the submit-prompt code sketch as part of CR-2 fix.

### MJ-20. Bridge slug `compute-market-bridge` creates a third-variant slug problem on top of CR-1

Phase 4 §E line 387: StepContext slug `"compute-market-bridge"`. §F line 428: DADBEAR semantic path `"bridge/{model_id}/{job_id}"` — NOT prefixed with the slug. Three different identifiers for the same bridge job. [A-M9]

**Fix:** After CR-1 is fixed, decide: is bridge a subset of `market:compute` (one slug, step_name distinguishes) or a separate slug (`market:compute-bridge`)? Pick one; apply consistently to StepContext, DADBEAR work item slug, semantic path, cost log slug.

### MJ-21. Admission control hold filter is unspecified

Phase 2 §III line 422 admission check: "active DADBEAR holds on `"compute-market"` slug." Which hold NAMES block dispatch? Phase 5 places `quality_hold`. Phase 6 places `measurement`/`suspended`/`escalation` (some marker, some gate). No enumeration. [A-M8]

**Fix:** Enumerate the hold filter explicitly: blocking = {`frozen`, `breaker`, `cost_limit`, `quality_hold`, `timing_suspended`, `reputation_suspended`, `suspended`, `escalation`}. Marker holds (e.g., `measurement`) do NOT block dispatch.

### MJ-22. AI Registry / management model tier is undefined but Phase 6 depends on it

Phase 6 §VI references "management model tier" and "AI Registry tier routing." CLAUDE.md mentions an "AI Registry (slot-based model routing)" in GoodNewsEveryone but no companion doc is listed. [C-C6]

**Fix:** Add the AI Registry doc (GoodNewsEveryone/docs/architecture/... or similar) to Phase 6's companion list. Inline the tier-routing contribution schema spec into Phase 6 §VI or require it as a prerequisite.

### MJ-23. Phase 3 `ItemCostEstimate::Cost` enum doesn't cover auto-commit items (Phase 6)

Phase 3 §III.5 reshapes `ItemCostEstimate` to `Cost::Usd(i64) | Cost::Credits(i64)`. Phase 6 §IV introduces sentinel auto-commit items that skip LLM dispatch. What `Cost` value do they carry? No answer. [C-M14]

**Fix:** Add `Cost::None` variant, or specify auto-commit items bypass preview gate entirely (which, per Phase 3, would be an exception worth documenting).

### MJ-24. Bootstrap mode operational policy is under-specified for launch reality

Architecture §III Bootstrap Mode says Wire acts as transient relay at launch. No spec on: concurrency cap (seams mentions "contribution-driven concurrency cap" without naming the contribution), failure retry, transition trigger to exit bootstrap. [C-M10]

**Fix:** Architecture §III add Bootstrap Operational Policy subsection: name the concurrency-cap contribution, specify failure retry, specify the metric that triggers exit from bootstrap (e.g., relay capacity above threshold for N days).

### MJ-25. `max_completion_token_ratio` economic_parameter defined in §XIV but no RPC reads it

Architecture §XIV line 1703 introduces `max_completion_token_ratio` with `ratio: 2`. Deployed `settle_compute_job` hardcodes `* 2`. No RPC reads the contribution. [C-M3, A-U2]

**Fix:** Phase 3 `settle_compute_job` migration reads `max_completion_token_ratio` from the contribution (falling back to 2 if absent). Deleted hardcoded `* 2`.

---

## Lesser Findings (consolidated)

**Overlay-body inconsistencies in storage/relay** (7 items from B-OB series): storage §V missing TunnelUrl column referenced in overlay; storage `wire_nodes.credits_earned_total` type ambiguity; storage `settle_document_serve_v2` signature breaks privacy claim (takes `consumer_operator_id` as input while overlay says node never sees it); storage `MarketOpportunity.best_provider_rate: u64` should be i64; relay Pillar 37 table overlaps with overlay additions; relay §VI.3 onion token `target_path` hardcoded to compute endpoint.

**Terminology drift** (10 items from C-T series): "Wire Compute Market" vs "compute market" vs "market:compute"; "bootstrap mode" vs "Wire-proxied" vs "standard tier"; `wire_job_token` vs `wire_document_token`; "privacy policy" vs "dispatch policy" vs "participation policy"; storage `storage_pricing` vs `PricingStrategy` vs `wire_storage_offers`; "relay chain" vs "relay network" vs "relay market"; `start_compute_job`'s prose says `started_at` but column is `dispatched_at` (already noted in docs but reappeared in prose); "fleet" vs "same-operator" vs "fleet-local-GPU"; "steward" vs "sentinel" vs "daemon" mixing.

**Pillar 37 cold-start sentinel hedging** (A-M11): Phase 2 queue mirror backoff (1s/2s/4s/30s) and Phase 3 delivery backoff (1s/64s/3) are labeled cold-start fallbacks. Adam's `feedback_pillar37_no_hedging` says no "reasonable default" exceptions. Current patterns need either (a) explicit contribution seeding before any code reads the policy, or (b) documented "Ask Adam" sentinel flags.

**Storage OB-findings consolidated:**
- OB-2: `settle_document_serve_v2` RPC takes `consumer_operator_id` as param; privacy claim says node never sees it. Signature must change: resolve operator from token_id inside RPC.
- OB-3: Relay plan `target_path` hardcoded to `/v1/compute/job-dispatch`; storage pulls need different path. Make `target_path` "per-market final endpoint."
- OB-4: `MarketOpportunity.best_provider_rate: u64` is credit amount, should be `i64` (Pillar 9).
- OB-5: Rotation param field-ownership collision — `rotation_interval_s` / `drain_grace_s` appear on both `privacy_policy` and `relay_delivery_policy`. Pick one home. Recommend: `privacy_policy` owns cadence (requester-affecting); `relay_delivery_policy` owns operational timing (operator-affecting).

---

## Cross-Auditor Agreement Ranking

Findings with 3+ auditor agreement (strongest signal):
- CR-1 (slug namespace) — A, B, C
- CR-2 (SOTA drift Phase 2/3) — A, A, C, C (quadruple)
- CR-5 (allow_market_dispatch missing) — A, B, D
- CR-6 (CallbackKind mismatch) — A, B, D

Findings with 2 auditor agreement:
- CR-3, CR-7, CR-8, CR-13, CR-14

Findings from single auditor (still valid, just less triangulated):
- CR-4, CR-9, CR-10, CR-11, CR-12, CR-15
- Most of the Major section

---

## Action Table (finding → target doc → fix)

| # | Finding | Target doc(s) | Minimal fix |
|---|---|---|---|
| CR-1 | Slug namespace | All 7 compute phase docs + seams + storage + relay | Pick `market:compute / market:storage / market:relay`. Grep all docs. Lock in arch §VI. |
| CR-2 | SOTA drift Phase 2/3 | Phase 2 §I, §VII, §VIII audit table; Phase 3 §III | Delete old "submit-prompt" flow from Phase 3 §III 253–308. Rewrite Phase 2 §I privacy paragraph to point at arch §III bootstrap mode. |
| CR-3 | clawback signature | compute-market-architecture §IX | Update arch §IX to Phase 5 signature `(p_job_id, p_verdict_id) RETURNS TABLE(...)`. |
| CR-4 | aggregate_compute_observations x3 | Phase 3 §II, Phase 5 §VIII, arch §IX | Rename to two distinct functions (`aggregate_compute_observations_for` + `refresh_offer_observations_sweep`). |
| CR-5 | allow_market_dispatch missing | Phase 2 §III + seams §VIII + project_async_fleet_shipped memory | Add to Phase 2 scope: extend struct + supersede bundled contribution + add mode→booleans projection. |
| CR-6 | CallbackKind mismatch | architecture §III + seams §VIII OR code rename | Pick convention. Either rename code `MarketStandard/Relay` → new names, or revert docs to `Fleet/MarketStandard/Relay`. |
| CR-7 | MarketDispatchRequest.messages shape | Phase 2 §III | Option 3: divergence documented + `messages_to_prompt_pair` helper spec'd. |
| CR-8 | fill_compute_job signature | Phase 2 §II migration list | Add `ALTER FUNCTION fill_compute_job` to add relay_count + requester_operator_id. |
| CR-9 | status CHECK constraint | Phase 5 §XVI | Clarify: add new CHECK covering all values, OR explicitly keep as free TEXT. |
| CR-10 | select_relay_chain 4 states | arch §IX, Phase 2 §II, seams §III migrations | No separate function; rejection inline in fill. Purge "stub" from seams and "dead code" from Phase 2. |
| CR-11 | open→paneling transition | Phase 5 §IX | Add `select_adjudication_panel(p_case_id)` RPC OR broaden `resolve` filter to `IN ('open', 'paneling')`. |
| CR-12 | min_replicas DEFAULT 2 | Storage §V | Drop NOT NULL DEFAULT. Make required-no-default. |
| CR-13 | ServiceDescriptor not shipped | Phase 2 §I prereqs + seams §VIII | Add WS1+WS2 three-objects as prerequisite. Correct "shipped" claim. |
| CR-14 | compute_result_outbox unspec'd | Phase 2 §III | Decide share-or-split. Full DDL + CAS helper set OR explicit reuse of fleet_result_outbox. |
| CR-15 | MarketIdentity check undefined | arch §III or Phase 2 §III | Spec MarketClaims shape + verifier checks + key source. |
| MJ-1 | Phase 3 outbox delivery worker | Phase 3 §III | Add Outbox Delivery Worker subsection. |
| MJ-2 | market_delivery_policy undefined | Phase 2 §II + Phase 3 §II | Full field list + seed YAML + registration mapping. |
| MJ-3 | start_compute_job no caller | Phase 2 §III | Specify GPU loop calls it before dequeuing. |
| MJ-4 | advance_market_rotator % 80 | arch §IX | Replace inline SQL with deployed version. |
| MJ-5 | handle predicate h.status vs released_at | arch §IX or §VIII.5 | Reconcile. |
| MJ-6 | cancel relay-fee NULL stub | Phase 3 §II | Raise exception OR implement before ship. |
| MJ-7 | Participation policy 5-doc divergence | fleet-mps-build-plan canonical list | All 10 fields canonical. Other docs reference. |
| MJ-8 | Relay overlay self-contradiction | Relay overlay line 74 | Fix to `relay_delivery_policy`. |
| MJ-9 | Hardcoded heartbeat intervals in storage/relay | Storage §VII, Relay §V | Contribution reads. |
| MJ-10 | Relay TunnelRotationState String | Relay §VI.3 | 2-line edit to TunnelUrl. |
| MJ-11 | storage_result_outbox not declared | Storage §V | Add DDL + sweep, OR drop overlay claim. |
| MJ-12 | Storage sync streaming vs outbox | Storage overlay + §VII | Pick one with size threshold. |
| MJ-13 | Build ordering over-serializes Storage | Seams §VIII | Storage S1 depends on Phase 1 only. |
| MJ-14 | FleetPeer.provider_types not in announce | Phase 4 §V.D | Extend HeartbeatFleetEntry + announce body. |
| MJ-15 | AppState/ServerState wiring unspec'd | Phase 2 §III | Add Boot Ordering subsection. |
| MJ-16 | DADBEAR preview short-circuit unspec'd | Phase 2 §V | Spec bypass-path INSERT fully. |
| MJ-17 | Queue mirror plumbing unspec'd | Phase 2 §III | Spec mpsc sender pattern + task owner + seq. |
| MJ-18 | wire_job_token issuance unspec'd | arch §IX or Phase 2 Wire | Spec signing path. |
| MJ-19 | submit-prompt ghost endpoint | Phase 3 §III | Subsumed by CR-2. |
| MJ-20 | bridge slug third variant | Phase 4 §E,§F | Subsumed by CR-1 resolution. |
| MJ-21 | Admission hold filter unspec'd | Phase 2 §III | Enumerate blocking hold names. |
| MJ-22 | AI Registry undefined | Phase 6 §VI | Add prerequisite doc OR inline schema. |
| MJ-23 | Cost::None variant for auto-commit | Phase 3 §III.5 OR Phase 6 §IV | Add variant OR spec bypass. |
| MJ-24 | Bootstrap mode policy | Architecture §III | Add Bootstrap Operational Policy subsection. |
| MJ-25 | max_completion_token_ratio not read | Phase 3 §II | Migration reads contribution. |

---

## Recommended Next Step

A focused **docs-only unification pass** hitting the critical findings in this order (prior work unblocks subsequent):

1. **CR-1** (slug namespace) — mechanical grep+replace once convention chosen
2. **CR-2** (SOTA drift) — delete old flows; mostly deletion, not new content
3. **CR-5** (participation policy field gap) — struct extension + projection + bundled JSON supersede
4. **CR-6** (CallbackKind naming) — pick convention, reconcile docs ↔ code
5. **CR-13** (ServiceDescriptor not shipped) — correct "shipped" claims, add prerequisite
6. **CR-14, CR-15, MJ-2** (outbox + MarketIdentity + market_delivery_policy) — the three unspecified foundations
7. **CR-3, CR-4, CR-8, CR-9, CR-10, CR-11, CR-12** — RPC signature / migration / table issues

Major findings will mostly clear themselves as byproducts of the critical fixes, or can be addressed in a second pass.

**Do not start implementation** until critical findings are resolved. The Phase 2 implementer's wanderer explicitly cannot build from the current doc set (D verdict). Phase 4 + Phase 6 + most of Phase 5 are implementable today with small friction — but implementing them while Phase 2/3 foundations are ambiguous creates integration pain later.
