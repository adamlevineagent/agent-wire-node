# Decision & Implementation Log — Three-Market Build

**Started:** 2026-04-16 evening (Adam handed off overnight)
**Covers:** doc unification → audit-until-clean → Phase 2 implementation → onward

Format: chronological entries. Each entry = timestamp + category + what was decided/done + rationale + consequences (anticipated or observed). Decisions also live canonically in other docs (architecture §VIII.6 for the DD-series); this log tracks them IN TIME with narrative context.

Categories: `DECIDE` (design fork closed), `APPLY` (decision propagated to docs/code), `DISCOVER` (finding made), `BUILD` (implementation landed), `VERIFY` (audit result), `PAUSE` (held for external input).

---

## Session 1: Doc Unification Pass

### 2026-04-16 ~21:30 · DECIDE · DD-A through DD-O logged to architecture §VIII.6

After running the 2026-04-16 audit (15 critical + 25 major findings) and identifying 5 systemic roots underneath, I wrote 15 design decisions to close every TBD / hand-wave / parallel-projection in the doc set. All decisions live in `compute-market-architecture.md` §VIII.6 "Design Decisions Log."

- **DD-A** — DADBEAR slug namespace = `market:compute` / `market:storage` / `market:relay`. Bridge uses compute slug + step_name `"bridge"`.
- **DD-B** — CallbackKind variants revert to shipped names `Fleet / MarketStandard / Relay` (docs were wrong; code is canonical).
- **DD-C** — `MarketDispatchRequest.messages: Value` diverges from fleet's two-string shape; `messages_to_prompt_pair` helper at `pyramid/messages.rs` handles the conversion on the provider side.
- **DD-D** — Market reuses `fleet_result_outbox` with extended `validate_callback_url` semantics (Fleet checks roster; MarketStandard/Relay accept any HTTPS because JWT-gated). No `compute_result_outbox`. Storage also reuses.
- **DD-E** — `market_delivery_policy` = full 18-field contribution parallel to `fleet_delivery_policy`. Absorbs `match_search_fee` / `offer_creation_fee` / `queue_push_fee` / `queue_mirror_debounce_ms` that were previously separate economic_parameter contributions (consolidation).
- **DD-F** — `MarketClaims` JWT shape: `aud/iss/exp/iat/sub/pid`. Verifier checks signature + aud + exp + pid-equals-self-node-id + sub-non-empty. Same Wire signing key as fleet JWTs; aud discriminates.
- **DD-G** — Wire's `POST /api/v1/compute/fill` issues the `wire_job_token` with 5min TTL (`fill_job_ttl_secs` economic_parameter, seeded Phase 2). `fill_compute_job` return type extended with `provider_node_id` so the route handler can populate `pid`.
- **DD-H** — Admission hold filter = enumerated blocking list (frozen, breaker, cost_limit, quality_hold, timing_suspended, reputation_suspended, suspended, escalation). Marker holds like `measurement` are non-blocking.
- **DD-I** — `compute_participation_policy` has 10 canonical fields. Mode projects to booleans (coordinator/hybrid/worker presets). Explicit booleans override mode.
- **DD-J** — Every RPC has exactly ONE canonical SQL body. Architecture §IX becomes a table + pointers. RPCs owned by later phases live in their phase doc's §II.
- **DD-K** — Handle predicate is `h.released_at IS NULL`, not `h.status = 'active'`. Docs match shipped code.
- **DD-L** — `wire_compute_jobs.status` and `wire_compute_offers.status` get CHECK constraints added in Phase 5 migration (covering all new quality/clawback values).
- **DD-M** — `settle_compute_job` reads `max_completion_token_ratio` from economic_parameter contribution; deletes hardcoded `* 2`.
- **DD-N** — Phase 5 ships `select_adjudication_panel(p_case_id)` RPC to close the `open → paneling` state-machine gap.
- **DD-O** — `wire_hosting_grants.min_replicas INTEGER NOT NULL` (no DEFAULT). Caller always supplies from policy.

**Rationale for the systemic approach over patching:** 13 of the 15 audit criticals reduce to 5 systemic roots (overlay-layering pattern, shipped-claim drift, parallel projections, TBD hand-waves, SOTA propagation failure). Patching 15 findings individually would grow the doc mass; fixing the roots collapses the findings. Only CR-9 (status CHECK — DD-L), CR-11 (paneling gap — DD-N), CR-12 (min_replicas default — DD-O) are genuine individual bugs not explained by a systemic root.

**Next:** apply DD-A through DD-O across all 10 plan docs.

---

## Session 1 continuing — log entries appended as applies happen.

### 2026-04-16 ~22:00 · APPLY · DD-A slug namespace sweep across all 10 plan docs

Mechanical sweep: `"compute-market"` → `"market:compute"`, `"storage-market"` → `"market:storage"`, `"relay-market"` → `"market:relay"`. Phase 5 and Phase 6 already used the `market:<kind>` convention; Phase 2, 3, seams §VIII, storage, relay were inconsistent. Phase 4's `compute-market-bridge` became `market:compute` with `step_name: "bridge/<job_id>"` (bridge is a step_name discriminator inside the compute slug, not a separate slug — preserves unified quality-hold propagation).

Verified clean: `grep -E '"compute-market"|"storage-market"|"relay-market"|compute-market-bridge'` returns only the intentional DD-A explanatory references.

### 2026-04-16 ~22:10 · APPLY · DD-B CallbackKind revert to shipped names

Thread B1 renamed `CallbackKind` variants in architecture §III + seams §VIII to `WireBootstrap/RequesterTunnel/RelayChain/FleetPeer`. Shipped code at `fleet.rs:582` has `Fleet { dispatcher_nid } / MarketStandard / Relay`. Reverted docs to match code. `MarketStandard` covers both "launch bootstrap" and "post-relay-market 0-relay direct" — same variant, different callback_url over time.

### 2026-04-16 ~22:15 · APPLY · DD-I participation policy 10-field canonical list

Rewrote `fleet-mps-build-plan.md` "Durable contribution" section to enumerate all 10 canonical fields (storage + relay included) with the mode→booleans projection rule. Added explicit WS1 gate: bundled schema_definition MUST include all 10 fields even for phases not yet building. Phase 2's §III participation-policy section simplified to reference the fleet-mps-build-plan canonical list rather than re-declare the fields. Storage and relay overlays' field tables also reference rather than re-declare.

Also corrected: participation policy on `local_mode.rs:1720` currently has 5 fields. The remaining 5 (`allow_market_dispatch`, `allow_storage_hosting`, `allow_storage_pulling`, `allow_relay_serving`, `allow_relay_usage`) ship as part of Phase 2's scope OR as a standalone fleet-MPS extension preceding Phase 2. Phase 2 §III prerequisites updated to flag this.

### 2026-04-16 ~22:25 · APPLY · DD-F MarketIdentity + DD-G wire_job_token issuance

Phase 2 §III now specs `MarketClaims` struct, `verify_market_identity` verifier at `pyramid/market_identity.rs`, and the Wire's `/api/v1/compute/fill` JWT-issuance flow. Single signing key shared with fleet; `aud` claim discriminates. `fill_compute_job` return type extended with `provider_node_id` so the API route can populate the `pid` claim. Seed `fill_job_ttl_secs` economic_parameter (300s default) lands in Phase 2 migration.

### 2026-04-16 ~22:35 · APPLY · DD-C messages_to_prompt_pair helper

Phase 2 §III now specs the `pyramid/messages.rs` module with the conversion helper. `MessagesError` enum lists specific error cases. Provider-side handler calls the helper between idempotent-outbox-insert and DADBEAR-work-item-creation. Phase 2 is single-turn only (assistant turns rejected); multi-turn is a future phase extension.

Inverse helper (in the requester-side `WireComputeProvider.call()`) constructs messages from (system_prompt, user_prompt) at the market provider boundary. The fleet dispatch path is untouched — it still carries system/user strings as before.

### 2026-04-16 ~22:45 · APPLY · DD-D fleet_result_outbox reuse across compute + storage

Both Phase 2 and Storage S1 (via storage overlay fold) reference the shipped `fleet_result_outbox` table. No parallel `compute_result_outbox` or `storage_result_outbox`. The `callback_kind` column discriminates; `validate_callback_url` extends to accept any HTTPS URL for `MarketStandard` / `Relay` variants (JWT-gated). The 14 existing CAS helpers at `db.rs:2332-2655` work unchanged.

Phase 3 §III gained an explicit "Outbox Delivery Worker" subsection (MJ-1 resolution) — the provider-side loop that reads `status='ready'` rows, POSTs to `callback_url`, transitions to `delivered`. Same sweep loop as fleet.

### 2026-04-16 ~22:55 · APPLY · DD-H enumerated admission hold filter

Phase 2 §III admission check now enumerates the blocking hold set: `frozen`, `breaker`, `cost_limit`, `quality_hold`, `timing_suspended`, `reputation_suspended`, `suspended`, `escalation`. Marker holds (`measurement` etc.) are non-blocking. Prevents accidental over-block from Phase 6's measurement holds.

### 2026-04-16 ~23:00 · APPLY · DD-J RPC canonical relocation in architecture §IX

Stripped `start_compute_job` / `cancel_compute_job` / `clawback_compute_job` / `aggregate_compute_observations` SQL bodies from architecture §IX. Left pointers to their canonical phase-doc locations. Architecture §IX now holds only the deployed RPCs (with Phase 2/3/5 patches documented inline).

The CR-3 / CR-4 class of finding (two incompatible signatures for one RPC) cannot happen with this discipline — there's only ever one canonical body per RPC, and it lives where the RPC is owned.

### 2026-04-16 ~23:05 · APPLY · DD-K handle predicate sweep in architecture §IX

Replaced all 3 occurrences of `h.status = 'active'` with `h.released_at IS NULL` in architecture §IX RPC bodies. Matches deployed code. No more divergence between doc and migration.

### 2026-04-16 ~23:10 · APPLY · DD-M max_completion_token_ratio contribution read

Architecture §IX `settle_compute_job` now reads the ratio from economic_parameter contribution; deleted hardcoded `* 2`. Phase 2 migration adds the seed. The guard becomes: `p_completion_tokens > v_job.max_tokens * v_max_ratio`.

### 2026-04-16 ~23:15 · APPLY · DD-N select_adjudication_panel in Phase 5

Phase 5 §IX now has the new RPC between `file_compute_challenge` (inserts `'open'`) and `resolve_compute_challenge` (requires `'paneling'`). Random selection among eligible operators, with reputation + stake filters. Adds prerequisite table `wire_compute_challenge_panelists` + `paneled_at` column on cases. API route `POST /api/v1/compute/challenges/:case_id/select-panel` triggers it. Seeds `minimum_panelist_stake` economic_parameter.

### 2026-04-16 ~23:20 · APPLY · DD-O min_replicas no default in storage

Storage §V `wire_hosting_grants.min_replicas` is now `INTEGER NOT NULL` with no DEFAULT. Grant creator always supplies from policy. Fixed the Pillar 37 violation the plan was supposed to be removing.

### 2026-04-16 ~23:25 · APPLY · OB-2 settle_document_serve_v2 signature fix

Storage §IV `settle_document_serve_v2` signature changed from `(p_token_id, p_consumer_operator_id, p_hosting_node_id, p_document_id, p_matched_rate)` to `(p_token_id, p_hosting_node_id, p_serve_latency_ms)`. Consumer, document, and matched_rate are now resolved from the token row inside the RPC via the `RETURNING *` on the UPDATE that redeems the token. Matches the SOTA privacy claim that the provider never learns consumer identity.

### 2026-04-16 ~23:30 · APPLY · Storage + relay overlays folded into body deltas

Replaced the "Refresh Overlay" sections at the top of both docs with a compact "2026-04-16 Unification Pass" block that points at architecture §VIII.6 for the shared decisions and summarizes storage/relay-specific deltas from the 2026-04-13 draft. No more overlay-vs-body pattern — implementers read the body as canonical. The body's specific contradictions with the overlay (e.g., `String` vs `TunnelUrl` in `TunnelRotationState`) were fixed in place.

### 2026-04-16 ~23:40 · APPLY · Phase 4 provider_types heartbeat/announce transport extension (MJ-14)

Phase 4 §V.D now specifies the full transport extension, not just the `FleetPeer` struct field: `HeartbeatFleetEntry` gains `provider_types`; `FleetAnnouncement` gains same; `update_from_heartbeat`/`update_from_announcement` reducers carry through; Wire heartbeat response populates from `wire_compute_offers.provider_type` aggregation; `find_peer_for_rule` grows `required_provider_type: &str` arg; `llm.rs` Phase A becomes two passes (local first, bridge fallback).

### 2026-04-16 ~23:50 · STATUS · Doc unification pass nearly complete; ready for re-audit

Every DD-A through DD-O applied across the 10 docs. Every CR-* from the 2026-04-16 audit addressed (CR-1 slug, CR-2 SOTA, CR-3 clawback signature, CR-4 observations signature, CR-5 participation policy gap, CR-6 CallbackKind, CR-7 messages shape, CR-8 fill signature, CR-9 CHECK, CR-10 select_relay_chain, CR-11 paneling, CR-12 min_replicas, CR-13 ServiceDescriptor prereq, CR-14 outbox, CR-15 MarketIdentity). Most MJ-* also covered as byproducts. A few loose ends remain (deployed-foundation reference doc — decided to skip in favor of architecture §VIII.5 being canonical; it already does the job).

Re-audit next.

---

## Session 2: Cycle 2 Audit Response

### 2026-04-17 early · VERIFY · Cycle 2 A landed — 6 critical + 10 major, all propagation failures from Cycle 1 apply sweep

Cycle 2 A auditor (compute internals scope) verified 9/15 Cycle 1 CR fixes as clean, flagged 5 as partially-applied (residue in phase-doc bodies), and found 6 new critical + 10 new major — all of them propagation failures. Systemic learning: the Cycle 1 unification pass updated decision homes (architecture §VIII.6) cleanly but phase-doc bodies + audit-correction tables at the bottom of phase docs weren't fully swept. Specifically:

- Phase 2 §I privacy paragraph + §VIII audit-correction rows kept "Wire-proxied dispatch" framing despite DD-A/DD-B removing that from the canonical model
- Phase 3 §III.5 step 5 still referenced `submit-prompt` despite §III intro declaring it removed
- Phase 3 §II + Phase 5 §VIII SQL bodies both DECLARED `aggregate_compute_observations` without the CR-4 rename (would collide at migration time)
- Phase 4 §V.A + seams §VIII still named `compute_result_outbox` despite DD-D folding everything into `fleet_result_outbox`
- Architecture §XIV + Phase 2 §III line 381 had duplicate economic_parameter surfaces for values DD-E absorbed into `market_delivery_policy` (§XIV was fixed pre-audit but Phase 2 §III reader path wasn't)
- seams §III Phase 2 migration list + Phase 2 §VIII audit table kept `select_relay_chain stub` / "dead code" language that DD-J deleted

### 2026-04-17 early · APPLY · All 16 Cycle 2 A findings addressed

- C1: Phase 2 §I privacy paragraph rewritten to reference architecture §III bootstrap mode (DD-A/DD-B/DD-D)
- C2: Phase 2 §VIII audit-table rows 803-805 updated to bootstrap-relay framing
- C3: Phase 3 §III.5 step 5 + §VIII rows 680-681 purged of `submit-prompt` references
- C4: Phase 3 §II renamed `CREATE FUNCTION aggregate_compute_observations` → `aggregate_compute_observations_for`; Phase 5 §VIII renamed `CREATE FUNCTION aggregate_compute_observations` → `refresh_offer_observations_sweep`; pg_cron job + audit-correction row updated; Phase 3 §II ownership note rewritten
- C5: Phase 4 §V.A line 241 changed to `fleet_result_outbox` + callback_kind note; seams §VIII line 681 collapsed to single table with callback_kind discriminator
- C6: Phase 2 §III line 381 reader path rewritten to `market_delivery_policy.queue_mirror_debounce_ms` with hot-reload note
- M1/M2/M3: architecture §IX `fill_compute_job` header + inline comment updated; seams §III Phase 2 migration list item 2 + migration-file comment block + "What Phase 2 must build" line all purged of stub language
- M4: seams §III Phase 3 migration list rewritten to name both split functions + their scopes
- M5: Phase 5 §VIII body rename + grant rename + audit-correction row rewritten
- M6: seams §III `file_compute_challenge` signature fixed to include `p_challenge_type`
- M7: Phase 3 cancel relay-fee NULL no-op replaced with `RAISE EXCEPTION` per feedback_no_deferral_creep — silent deferral is now structurally impossible
- M8: Follow-on to C6, already fixed
- M9: Follow-on to C5, already fixed
- M10: Phase 2 §I prereqs rewritten to explicitly list async-fleet-dispatch + Fleet MPS WS1 (10 fields) + Fleet MPS WS2 (three objects)

### 2026-04-17 ~xxx · STATUS · Awaiting Cycle 2 C + D results (B landed + addressed)

### 2026-04-17 early · VERIFY · Cycle 2 B landed — 4 new critical + 7 new major (cross-market + storage + relay scope)

Cycle 2 B scope was seams §VIII + storage + relay + fleet-mps-build-plan. Most Cycle 1 fixes verified clean; structural overlay-to-body fold verified complete. But 11 new findings, all propagation failures from Cycle 1 apply sweep missing sibling sites, peripheral docs, or pre-existing Pillar 37 hedges that weren't explicitly in the DD sweep target list.

Key patterns the auditor called out as root cause:
1. Single-doc edits that cited a DD but forgot a sibling site in the same file (NC-4 handle predicate, NM-4 u64 fields). The first site got fixed; the second was missed.
2. Cross-doc decisions that landed in 3 of 4 docs (NC-3 outbox in seams, NC-2 build ordering in seams). Seams §VIII is the most frequently missed because it's treated as "reference" rather than "primary edit target."
3. New claims in the fold prose that didn't propagate to sibling bodies (NM-7 RelayServiceDescriptor invented without fleet-MPS or storage-body extension).
4. No Pillar 37 hardcoded-number grep was in the DD sweep target list (NM-4 u64 credit fields, NM-5 `interval '2 minutes'` in relay, NM-6 `100 MB` hedging without seed).

### 2026-04-17 early · APPLY · All 11 Cycle 2 B findings addressed

- NC-1: Deleted "new Storage variant on CallbackKind" clause from storage line 18; storage now explicitly reuses `CallbackKind::MarketStandard` per DD-B.
- NC-2: Rewrote seams §VIII build-ordering graph. Storage S1 is now a Phase 1 dependent, parallel to Compute Phase 2 (not downstream). Updated parallelization prose.
- NC-3: Already fixed in Cycle 2 A response (auditor read pre-fix state).
- NC-4: Fixed `h.status = 'active'` → `h.released_at IS NULL` in both storage §IV line 291 and relay §III line 265 (DD-K complete now).
- NM-1: Added `POST /api/v1/storage/settle` route to storage §VII endpoints table.
- NM-2: Added `wire_nodes.credits_earned_total` column ALTER to storage §V modifications; clarified `wire_graph_fund` CHECK is already Phase 1-extended prospectively.
- NM-3: Fixed broken §VI.G cross-reference to §VI "Chunked Storage for Large Files".
- NM-4: Changed `best_provider_rate: u64` + `grant_payout_rate_per_day: u64` → `i64` in storage §VI MarketOpportunity. Fixed downstream arithmetic (`u64 → i64` casts to keep types aligned).
- NM-5: Replaced hardcoded `interval '2 minutes'` in relay §V `select_relay_chain` with `staleness_thresholds.heartbeat_staleness_s` contribution read. Also fixed relay §II prose "<2 minutes" → cite contribution; storage §VII "heartbeat fresh" → cite contribution.
- NM-6: Added `sync_stream_max_bytes` seed to Storage S1 Wire workstream migration list. Also enriched the list to note `wire_graph_fund` CHECK is Phase 1 pre-extended and to call out the new `POST /api/v1/storage/settle` route and the OB-2 signature fix.
- NM-7: Deleted "RelayServiceDescriptor parallel to StorageServiceDescriptor" claim; replaced with simpler "relay offers derive from relay_pricing contribution + RelayMarketState runtime state" that the body actually implements.

### 2026-04-17 early · VERIFY · Cycle 2 C landed — 1 critical + 3 major, all overlapping with Cycle 2 A + B findings I already addressed

Auditor C re-read the file state while I was mid-fix-pass; most findings (CR-F1, MJ-F1, MJ-F2, MJ-F3-outbox, MJ-F4) were already closed when C ran. Two NEW issues surfaced:
1. **StorageIdentity / RelayIdentity in seams §VIII were not formally authorized by a DD** — my storage + relay plans mention them but no DD in §VIII.6 covers them. Extended DD-F to add the "Storage + Relay parallel verifiers" subsection covering all three markets under one decision.
2. **DD-E claimed "18 fields parallel to `fleet_delivery_policy`" but seed enumerated 16** — actual fleet_delivery_policy has 18 fields; market has 12 shape-shared fields + 4 market-specific = 17 including version. Corrected DD-E from "full 18-field parallel" to "shape-parallel with market-specific field set" and listed exactly which fleet fields are dropped and why.

Also from C's brain dump: bridge hold scoping under DD-A. Bridge is `market:compute` + `step_name: "bridge/..."`. When Phase 5 places a quality_hold on slug `market:compute`, does it apply to bridge work items? Ambiguous. Added **DD-P: Bridge hold scoping via step_name_prefix** — default hold covers both local-GPU and bridge; targeted hold uses a new optional `step_name_prefix` column on `dadbear_holds`. Minimal schema change; UX can offer "scope: all | local-only | bridge-only".

### 2026-04-17 early · APPLY · All Cycle 2 C new issues addressed

- DD-F extended with "Storage + Relay parallel verifiers" paragraph.
- DD-E rewritten to honestly describe the field set (shape-parallel, not full-18-field parallel).
- DD-P added for bridge hold scoping.

### 2026-04-17 early · STATUS · Cycle 2 A + B + C addressed; waiting for D (implementer wanderer)

### 2026-04-17 early · VERIFY · Cycle 2 D landed — 1 hard blocker (N1) + 5 soft + 6 friction items. Verdict flipped from "No" to "Qualified Yes."

D re-read Phase 2 as an implementer about to build tonight. B1-B5 from Cycle 1 all resolved at spec level (MarketIdentity, messages helper, outbox decision, market_delivery_policy, participation-policy prereq gap). ONE hard blocker surfaced (N1) — and it's a genuine ground-truth failure in my Cycle 1 work:

**N1 — DD-D rested on three false assumptions about shipped code:**
1. "schema unchanged from async-fleet-dispatch" — FALSE. `fleet_result_outbox` at `db.rs:2271-2290` has 13 columns, NONE of them `callback_kind`. I'd never read the shipped DDL.
2. "dispatcher_node_id = Wire's node_id (from `AuthState.self_node_id`)" — FALSE. AuthState has `node_id` (THIS node's Wire-assigned ID) but no field for the Wire's own ID.
3. "`validate_callback_url` semantic extension is a single-site change" — MISLEADING. `fleet.rs:659-661` currently returns `KindNotImplemented` for MarketStandard/Relay; the actual accept-any-HTTPS logic has to be written.

Pattern: I wrote DD-D based on my memory of the async-fleet-dispatch PLAN doc, not by reading the shipped code. `feedback_verify_prior_infra_upfront.md` exactly warns against this; I violated my own memory. Also updated `reference_async_dispatch_pattern.md` to carry a prominent CAUTION block distinguishing shipped state from target state.

### 2026-04-17 early · APPLY · DD-Q pre-flight migration pack closes N1

Added DD-Q covering:
1. ALTER `fleet_result_outbox` ADD COLUMN `callback_kind TEXT NOT NULL DEFAULT 'Fleet'` + index. Backfills all existing rows correctly (all are Fleet today).
2. Update sweep helpers in `db.rs` to SELECT `callback_kind` and reconstruct `CallbackKind` for revalidation on orphan promotion.
3. Extend `validate_callback_url` at `fleet.rs:659-661` with accept-any-HTTPS logic for `MarketStandard | Relay` (scheme=https, host non-empty). New error variants `SchemeNotHttps`, `MissingHost`.
4. Dispatcher sentinel `"wire-platform"` for market outbox rows (no `AuthState` extension needed).
5. DDL for `pyramid_market_delivery_policy` singleton table in `db::init_pyramid_db`, parallel to fleet's.

Added to Phase 2 §II as "Workstream 0 — DD-Q pre-flight migrations" (apply before any handler work). ~80 lines of code total across ALTER + validate extension + new module.

Updated DD-D to cross-reference DD-Q and honestly describe what was falsely asserted.

### 2026-04-17 early · APPLY · Soft-item fixes from Cycle 2 D

- **N2** three-objects spec gap: Phase 2 §III already flags as prereq; marking as "build with judgment if Fleet MPS WS2 hasn't shipped first." Not changing the plan — this is acknowledged scope elasticity.
- **N3** `compute_market_state.json` path: specified `${app_data_dir}/compute_market_state.json` pattern, added `schema_version` field + cold-start rebuild on version mismatch.
- **N4** platform-operator resolution query: SQL is trivial enough that Phase 2 §II migration item 1 note is sufficient; no inline expansion needed.
- **N5** `queue_mirror_backoff_schedule` "name TBD": collapsed into `market_delivery_policy.backoff_base_secs` + `backoff_cap_secs` per DD-E. No separate contribution needed.
- **N6** DADBEAR state mismatch: Phase 2 §VI verification criterion #4 rewritten to say `state = "previewed"` matching §V's P3 fix.
- **F1** validate_callback_url extension: explicit in Phase 2 Workstream 0.3.
- **F2** wire_node_id registration: N/A per DD-Q part 4 sentinel approach.
- **F3** `compute_market_surface` IPC: added to §III IPC list.
- **F4** same as N6: fixed.
- **F5** seed location split: added explicit "Seed location convention" subsection to Phase 2 §II — node-bundled vs Wire-seeded by reader side.
- **F6** "14 CAS helpers work unchanged" framing: updated to acknowledge the sweep-reader changes per DD-Q part 2.

### 2026-04-17 early · VERIFY · Final self-audit sweep clean

Grep check across all 10 docs: no residual `compute-market`/`storage-market`/`relay-market` slugs outside DD-A explanatory references. No `submit-prompt`, no `compute_result_outbox`, no `WireBootstrap`/`RequesterTunnel`/`RelayChain` variants. No `h.status = 'active'` outside DD-K itself. No old `aggregate_compute_observations(` signature. No `select_relay_chain stub` / `dead code in Phase 2` framing. DD-Q referenced 5 times in Phase 2 §II migrations block.

Per `feedback_direct_over_delegation_small`, skipping a third delegated audit cycle and instead doing the final sweep manually — auditor D explicitly recommended this (it's in D's verdict). The remaining gaps surface during implementation, which is the cheaper place to find them than yet another doc pass.

### 2026-04-17 early · STATUS · Plan set ready to commit + start Phase 2 implementation

All critical findings (Cycle 1 + Cycle 2) addressed. All soft gaps addressed or acknowledged. DD log + friction log current. Next: git commit on main, create feature branch `feat/compute-market-phase-2`, begin Workstream 0 (DD-Q pre-flight).


Cycle 2 A (compute internals) and B (cross-market + storage + relay) both fully addressed. Two more auditors still running (fresh read C, implementer wanderer D). Will integrate on landing and run Cycle 3 if any new findings surface.

---

## Session 2: Phase 2 Workstream 0 Implementation

### 2026-04-17 · BUILD · WS0 pre-flight shipped (commit 6e414bf)

Landed the DD-Q pre-flight migrations on feature branch
`feat/compute-market-phase-2`. Infrastructure-only, no market behavior:

1. **`validate_callback_url` MarketStandard/Relay branch** — was
   `KindNotImplemented` placeholder; now validates https + non-empty host.
2. **`fleet_result_outbox.callback_kind` column** — added with DEFAULT
   `'Fleet'` in CREATE, conditional PRAGMA-guarded ALTER for existing DBs.
3. **Sweep helpers filtered by `callback_kind = 'Fleet'`** — sweep_expired,
   retry_candidates, startup_recovery gated so market rows land in Phase 2
   WS1+'s own worker.
4. **`pyramid_market_delivery_policy` singleton table** + Rust struct +
   seed YAML, 17 fields shape-parallel to FleetDeliveryPolicy with the 4
   economic-gate fees absorbed per DD-E. `default_matches_seed_yaml` test
   enforces the coincidence.
5. **Helpers**: `WIRE_PLATFORM_DISPATCHER` sentinel, `callback_kind_str` /
   `callback_kind_from_str` / `CallbackKindColumn` for outbox round-trip.
6. **Dead variant removed**: `KindNotImplemented` and its 3 sites deleted.

Test delta: +9 passing (7 market_delivery_policy + 2 replaced callback
tests), 0 regressions. 15 pre-existing DADBEAR/staleness failures unchanged.

### 2026-04-17 · VERIFY · WS0 serial verifier pass (commit 7b303c0)

Ran serial verifier (one focused agent, full WS0 spec context, told to fix
in place). Caught THREE issues:

- **MAJOR** — `fleet_outbox_count_inflight_excluding` missed the
  `callback_kind = 'Fleet'` filter. Fleet's `max_inflight_jobs` budget
  would have been consumed by market rows and vice versa (cross-market
  starvation). Fixed + regression test
  `test_fleet_outbox_count_inflight_ignores_market_rows`.
- **MAJOR** — `fleet_outbox_expire_exhausted` missed the same filter. A
  market row that hit Fleet's `max_delivery_attempts` would have its
  `expires_at` pushed into the past but never be collected by the Fleet-
  scoped sweep (orphaned row). Fixed + regression test
  `test_fleet_outbox_expire_exhausted_ignores_market_rows`.
- **MINOR** — no roundtrip test for `callback_kind_str` /
  `callback_kind_from_str`. The strings MUST stay byte-aligned with the
  SQL filter literals or sweeps silently stop picking up rows. Added
  `callback_kind_str_roundtrips_all_variants`.

Plus **spec alignment**: tightened `validate_callback_url` to https-only
(my commit accepted http for dev rigs with a comment; canonical DD-Q part 3
and architecture §VIII.6 say `!= "https"`). Updated the test fixture to
assert http is now rejected with `SchemeNotHttps`. If dev rigs need
non-TLS later, that's a new `allow_http_callbacks` field on
`market_delivery_policy`, not an inline loosening.

### 2026-04-17 · VERIFY · WS0 wanderer pass (commit fba3723)

Ran wanderer — no punch list, just "does this actually work when WS1+
builds on it." Caught THREE integration gaps (plus one wrong comment):

- **GAP 1** — `config_contributions.rs` had no `"market_delivery_policy"`
  arm. Supersession would hard-fail with `UnknownSchemaType`. Added the
  arm shape-parallel to the fleet sibling.
- **GAP 2** — `main.rs` had no first-boot seed path for the contribution.
  Operators tuning the policy for the first time would find nothing to
  supersede; no `contribution_id` tracked for the Wire-sync overlay.
  Added parallel to the fleet block at `main.rs:11920`.
- **GAP 3** — ConfigSynced reload branch (deferred). Would require the
  yet-unconstructed Phase 2 WS1+ `MarketDispatchContext` to hold the
  `Arc<RwLock<MarketDeliveryPolicy>>`. Documented inline so WS1+ knows
  where to wire it; not added here.
- **COMMENT** — `fleet.rs:678-681` was half-wrong: claimed both scheme +
  host checks were defense-in-depth because `TunnelUrl::parse` blocks
  both. Truth: `TunnelUrl` accepts http, so `SchemeNotHttps` is in fact
  the only layer enforcing https; only the host check is defense-in-
  depth. Comment corrected.

Added `test_fleet_outbox_pre_ws0_alter_upgrade_path` — simulates the
upgrade path for every existing node (build pre-WS0 schema without
`callback_kind`, insert legacy row, run `init_pyramid_db`, verify column
added, legacy row backfilled as 'Fleet', idempotent on re-run).

**This test surfaced a real ordering bug**: the
`CREATE INDEX idx_fleet_outbox_callback_kind` was in the same execute_
batch as the CREATE TABLE. On a pre-WS0 DB (table exists without
callback_kind), SQLite executed the CREATE INDEX first and errored with
"no such column: callback_kind" before the PRAGMA-guarded ALTER could
run. Moved the CREATE INDEX to a separate execute_batch AFTER the ALTER
guard. Without this test the regression would have silently broken every
operator upgrade in the field.

Added parallel config_contributions tests:
`test_sync_market_delivery_policy_writes_operational_table` +
`test_sync_market_delivery_policy_overwrites_on_resync`.

### 2026-04-17 · STATUS · WS0 complete, ready for WS1+

Three commits on `feat/compute-market-phase-2`:
- `6e414bf` — infrastructure
- `7b303c0` — verifier pass (2 major filter bugs + scheme tighten + roundtrip)
- `fba3723` — wanderer pass (contribution supersession + ALTER ordering)

Net test delta: +14 passing (7 market_delivery_policy + 2 market
config_contributions + 1 pre-WS0 upgrade + 1 roundtrip + 1 inflight
isolation + 1 expire isolation + 1 test replaced 2 tests). 0 regressions.
cargo check clean with default target (catches main.rs Send errors per
`feedback_cargo_check_lib_insufficient_for_binary`).

Next up: WS1 planning.


