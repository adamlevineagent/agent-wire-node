# Walker Re-Plan Wire 2.1 — Implementation Log

Append-only log of what's done. Newest at top. Updated at every commit.

**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3
**Handoff:** `docs/plans/walker-re-plan-wire-2.1-HANDOFF.md`
**Branch:** `walker-re-plan-wire-2.1`
**Started:** 2026-04-21 (template commit; Wave 0 task 1 lands next)

---

<!--
Entry template:

## <YYYY-MM-DD HH:MM> — commit <sha> (branch <name>)

**Plan task:** Wave X task N — <short label>
**Changed:** <1-2 sentences on what changed and where (file:line).>
**Cargo check:** clean (default target) / errors — <summary>
**Cargo test:** <module/test names> — <N/N pass>
**Deviation:** None / <rationale if any>
-->

## 2026-04-21 — Wave 2 tasks 13-17 — commits df08ab9 + b99302b (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 2 tasks 13-17 — fleet branch inlined.
**Changed:**
- `src-tauri/src/pyramid/llm.rs` (commit df08ab9, +721/-537):
  - New `dispatch_fleet_entry` helper + `FleetDispatchArgs` bundle
    (~280 LOC). Takes already-validated fleet_ctx / policy_snap /
    callback_url / roster_handle / peer / jwt / rule_name /
    job_wait_secs; returns `Result<LlmResponse, EntryError>`.
    Three-tier classification per §4.1: Success → Ok; peer-ran-and-
    failed → RouteSkipped; timeout / orphaned → Retryable; dispatch
    POST is_peer_dead / 503 / other → RouteSkipped. Fleet never
    returns CallTerminal — failures never doom a call.
  - Walker fleet branch: runtime gate (branch_allowed +
    !skip_fleet_dispatch + rule_name + fleet_ctx + tunnel Connected
    + fleet_roster), non-blocking peer lookup, dispatch via helper,
    Ok path writes audit + EVENT_WALKER_RESOLVED with
    `options.dispatch_origin.source_label()` — same source-label
    feed as pool branch.
  - Deleted Phase A pre-loop (llm.rs 1776-2266) + `fleet_filter`
    retain. Market still has wave2_market_not_implemented stub.
  - `skip_fleet_dispatch` downgraded to secondary override (reason
    slug `fleet_replay_guard`); primary is `branch_allowed(Fleet)`.
  - Wave 1 fleet+market test renamed to wave2 variant; asserts all
    3 entries exhaust (fleet walks now instead of being pre-filtered).
  - 3 new fleet-branch tests:
    walker_fleet_branch_advances_on_no_peer,
    walker_fleet_branch_respects_skip_fleet_dispatch,
    walker_fleet_branch_respects_branch_allowed.
  - Dropped `mut` from `resolved_route` (the retain was the only
    mutation).

- `src-tauri/src/pyramid/dispatch_policy.rs` (commit b99302b,
  +59/-3): `resolve_local_for_rule` now filters both walker
  sentinels (fleet + market). New test
  `resolve_local_for_rule_filters_market_sentinel` (route [fleet,
  market, ollama-local] → only ollama-local resolves).

**Cargo check:** clean (default target). 72 warnings, identical to
  pre-Wave-2 baseline (pre-existing dead code in dadbear_* + deprecated
  tauri_plugin_shell::Shell::open).
**Cargo test:** `cargo test --lib` — 1765 pass / 15 fail (pre-existing).
  +4 net passes (3 walker fleet tests + 1 dispatch_policy test),
  0 new failures. Wave 1 test renamed + assertion flipped from
  "2 entries" to "3 entries"; still green.
**Deviation:** Merged Wave 2 plan suggested-commits 1+2 into a single
  refactor commit. Running the helper extraction as a separate commit
  would have left the helper dead-code transient between commits
  because Phase A pre-loop stayed untouched in the plan's incremental
  sequence; combining lets the walker call the helper and lets Phase A
  die in the same atomic change. Plan acceptance note explicitly
  permits this ("OR combine the first two if the incremental split
  creates transient dead code").

  Reason-slug classifications: timeout → Retryable (plan table says
  Retryable but notes "OR RouteSkipped" with walker-friendly default;
  chose Retryable to honor the plan's default slug + for distinct
  chronicle telemetry). Orphaned → Retryable (same rationale — walker
  advances regardless). peer_dead / 503 / other-dispatch-POST-fail →
  RouteSkipped (fleet failures never doom the call). JWT empty →
  RouteSkipped with reason `jwt_unavailable`. All tier decisions match
  the §4.1 error classification table; walker behavior is identical
  either way (both Retryable and RouteSkipped → advance).

  Helpers/vestiges: no Phase A-only helper became dead. The helper
  closures (`spawn_chronicle`) are new-local. No standalone functions
  in llm.rs were exclusive to Phase A. The old Phase A chronicle
  events (fleet_dispatched_async, fleet_result_received,
  fleet_result_failed, fleet_dispatch_timeout, fleet_dispatch_failed,
  fleet_peer_overloaded) all still fire from `dispatch_fleet_entry`;
  Wave 5 will sweep the fleet_* vs network_* vocabulary.

  Phase A grep: `Phase A` now appears only in test comments + unrelated
  modules (main.rs, server.rs, dadbear_*, public_html/*) where it
  references their own local phases — no live llm.rs references after
  the deletion.

## 2026-04-21 — Wave 1 tasks 8-10 verifier pass — commit 37ff562 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 1 verifier pass — walker body correctness audit + hygiene fixes.
**Verified clean:**
- Phase A (fleet pre-loop ~1433-1725) and Phase B (market pre-loop ~1732-2082) left UNTOUCHED by Wave 1 diff (`git diff da67787 42b5366 -- llm.rs` hunks start at line 2363).
- Compute_queue enqueue still runs before walker. `escalation_timeout_secs` struct field stays on `dispatch_policy` but is never dereferenced in runtime code (only one remaining use is the comment at llm.rs:2459). No `tokio::time::timeout(...pools.acquire...)` wraps anywhere.
- All 12 `emit_walker_chronicle` call sites use `&walker_source_label` (from `options.dispatch_origin.source_label()`) — no hardcoded "local" / "network" strings. All 7 Wave 1 events fire: `EVENT_WALKER_RESOLVED`, `EVENT_WALKER_EXHAUSTED`, `EVENT_NETWORK_ROUTE_{SKIPPED,SATURATED,UNAVAILABLE,RETRYABLE_FAIL,TERMINAL_FAIL}`.
- Three audit exit outcomes per plan §8 task 11 land correctly: Success → `complete_llm_audit(..., Some(winning_entry.provider_id))`; CallTerminal → `fail_llm_audit(audit_id, reason, last_attempted_provider_id.as_deref())`; Exhaustion → `fail_llm_audit(audit_id, "no viable route", None)`. `last_attempted_provider_id` is ONLY written after the fleet/market skip branch advances + pool classify_branch confirmed (line 2541), so wave1_not_implemented skips correctly do NOT pollute it.
- HTTP retry loop relocation intact: per-request timeout scaling (local_timeout_scale=5 for OpenaiCompat), exponential backoff on retryable statuses, context-exceeded 400 cascade (primary → fallback_1 → fallback_2) loops on SAME entry via `attempt += 1; continue`, `augment_request_body` + `parse_response` + provider-health hooks + cache store on success all present. Terminal-code classification: 401/403 → RouteSkipped, 404 → CallTerminal, 400 non-context exhausted → CallTerminal, 5xx exhausted → Retryable.
- Test coverage: `walker_exhausts_when_no_entry_viable`, `walker_skips_fleet_and_market_entries_in_wave1`, `walker_advances_on_pool_saturation` all exercise the real walker body (no mocks); saturated test uses `concurrency=0` real `ProviderPools`; exhaustion test asserts "no viable route" reaches caller.

**Fix-in-place changes (commit 37ff562):**
1. Removed cryptic `let _ = (&mut provider_impl, &mut secret, &mut provider_type);` no-op at llm.rs:2443. The outer bindings are never reassigned after `build_call_provider` returns — the `mut` qualifiers were vestigial from Phase D. Dropped `mut`, dropped the no-op; prefixed `_provider_impl` + `_secret` so the reader sees at a glance that only `provider_type` + `provider_id` are still read (by Phase A + queue-enqueue `should_enqueue_local_execution` check at line 871).
2. Deleted `maybe_fail_audit` (was `#[allow(dead_code)]` with a "reuse in Waves 2-3" comment). Waves 2-3 inline fleet + market INTO the walker, which calls `fail_llm_audit` directly with `last_attempted_provider_id`. The helper's provider-id-less signature doesn't match what the walker needs. Killing now rather than ambiguously deferring.
3. Tightened comment on `last_attempted_provider_id`'s `#[allow(unused_assignments)]` — the allow IS needed (confirmed by removing it and seeing the warning re-fire) because the walker can exhaust without any pool attempt (fleet/market-only routes) in which case the write-before-exhaust is never read.

**Cargo check:** clean (default target, `cargo check` — includes main.rs per `feedback_cargo_check_lib_insufficient_for_binary`). Lib warnings 71 → 69 after hygiene fixes (two `unused variable` warnings dropped via underscore prefix).
**Cargo test:** `cargo test --lib` — 1761 pass / 15 fail (pre-existing set exactly, no regression). `cargo test --lib walker` — 6/6 pass.
**Deviation:** None. One audit-level observation moved to friction log: the hoisted `_provider_impl` + `_secret` outer bindings are genuine fallback-only code; they can be deleted entirely once Wave 5 kills the `resolved_route = None` path (plan §5 deprecation enforcement). Low priority, not structural.

Wave 1 verifier clean; wanderer unblocked.

## 2026-04-21 — commits 6b83a86 + 42b5366 + ef51f7a (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 1 tasks 8 + 9 + 10 — walker loop replaces Phase D; HTTP retry relocated into pool-provider branch; `try_acquire_owned` abstraction with plan §3 error taxonomy.

**Changed (3 atomic commits):**
- `6b83a86` — `src-tauri/src/pyramid/compute_chronicle.rs`: 19 `EVENT_*` constants per handoff §"Chronicle event constants to add" (`EVENT_WALKER_RESOLVED`, `EVENT_WALKER_EXHAUSTED`, `EVENT_NETWORK_ROUTE_{SKIPPED,SATURATED,UNAVAILABLE,RETRYABLE_FAIL,TERMINAL_FAIL,…}` + 12 more deferred-emission constants). Gated `#[allow(dead_code)]` until Waves 2-4 wire up remaining emitters.
- `42b5366` — `src-tauri/src/pyramid/llm.rs`: walker loop + helpers. +864 / -545 LOC.
  - New `emit_walker_chronicle()` + two thin helpers above `struct NetworkHandleInfo` — unified fire-and-forget chronicle emitter for all walker events.
  - Former Phase D escalation block (~2366-2416) + former shared HTTP retry loop (~2485-2969) collapsed into a per-entry walker over `resolved_route.providers` (or a synthetic single-entry fallback when no route is configured, preserving pre-walker tests + pre-init behavior).
  - Provider impl re-instantiated per entry from the registry (or `build_call_provider()` fallback); the outer hoisted `provider_impl` / `secret` / `provider_type` from llm.rs:1361 are now only used to seed the synthetic fallback entry. Resolves the ownership conflict the plan flagged (prompt: "per-entry provider-trait instantiation").
  - `tokio::time::timeout(…, pools.acquire(…))` wrap retired; `try_acquire_owned` is non-blocking and `AcquireError::{Saturated, Unavailable}` advance immediately.
  - HTTP retry loop wrapped in `'http: { loop { … break 'http Err(EntryError::…) } }` — terminal conditions raise three-tier `EntryError` rather than bubbling with `return Err`. 401/403 = `RouteSkipped`; 404 = `CallTerminal`; 400 non-context-exceeded terminal = `CallTerminal`; other terminal or retry-exhausted = `Retryable`. Context-exceeded 400 cascade still loops SAME entry via `use_model` mutation.
  - Audit exit per plan §8 task 11: success → `complete_llm_audit(…, Some(winning_entry.provider_id))`; `CallTerminal` → `fail_llm_audit(…, last_attempted_provider_id)`; exhaustion → `fail_llm_audit(…, "no viable route", None)`.
  - Chronicle source label derived from `options.dispatch_origin.source_label()` — NOT hardcoded `"network"` or `"local"`.
  - `maybe_fail_audit` helper at llm.rs:~3287 kept with `#[allow(dead_code)]`; its former single caller (the old HTTP retry block) is gone, but Waves 2-3 may reintroduce fleet/market bubble paths that need it.
- `ef51f7a` — three `#[tokio::test]` walker tests in `pyramid::llm::tests`:
  - `walker_exhausts_when_no_entry_viable` — one unknown pool entry → Unavailable → exhaustion.
  - `walker_skips_fleet_and_market_entries_in_wave1` — `[fleet, market, unknown-pool]`; fleet is pre-filtered by the legacy Phase A filter (Wave 1 intermediate state), walker sees 2 entries (market + unknown), both skip/unavailable, exhausts with "2 entries" in the error string. Test doc-comments call out that Wave 2 raises this to 3.
  - `walker_advances_on_pool_saturation` — pool concurrency=0 → permanently saturated → walker advance → exhaustion.

**Cargo check:** clean (default target, `cargo check` from `src-tauri/`). 72 warnings total (71 lib + 1 bin) — same count as post-chronicle-constants baseline; walker surgery introduced zero new warnings. No `warning:` rows against `src/pyramid/llm.rs` or `src/pyramid/compute_chronicle.rs`.

**Cargo test:** `cargo test --lib walker_` — 3/3 walker-tests pass (plus 3 pre-existing walker-named tests from Wave 0). Full suite `cargo test --lib` — 1761 passed, 15 failed. Delta vs Wave 1 task 11 baseline (1758 + 15): +3 new walker tests, zero regressions on the pre-existing 1758. The 15 pre-existing failures are the same tracked set (yaml_renderer, etc.) untouched by this surgery.

**Deviations from plan:**
- Plan describes the walker as a simple `for (i, entry) in route.providers.iter()` loop. Implementation materializes `walker_entries: Vec<RouteEntry>` once so the no-route / empty-route case can synthesize a single-entry fallback without duplicating the loop body. The plan's §3 pseudocode assumed `route` was always present; the existing dispatcher supports a no-route path (tests, pre-init) that needs preservation.
- Plan §4.3 gate order was "branch_allowed → acquire → dispatch." Implementation does provider re-instantiation BEFORE `try_acquire_owned` so that credentials_missing surfaces as an Unavailable-reason before we touch the semaphore. Net walker semantic unchanged (both advance with the same chronicle event); the order swap just avoids holding a permit we don't need.
- Context-exceeded cascade in the HTTP retry used to `continue` the outer `for attempt` loop; the walker's `loop { … attempt += 1 }` body replicates this by incrementing `attempt` and `continue`-ing. Behavior identical; the explicit counter is because `break 'http` requires named-block syntax over a `loop` not a `for`.

**Known Wave 1 intermediate state** (Waves 2-3 close):
- Phase A fleet pre-loop (llm.rs:1248-1725) STILL RUNS before the walker — unchanged per Wave 1 contract. Fleet entries are filter-retained out of `route.providers` at llm.rs:1869 before the walker iterates. When Wave 2 lands, fleet becomes a real walker branch and the pre-filter + pre-loop disappear.
- Phase B market pre-loop (llm.rs:1732-2082) STILL RUNS — unchanged. Market entries in `route.providers` reach the walker and trigger `wave1_not_implemented` skip (but market dispatch already happened via Phase B, so this is a no-op). Wave 3 closes this duplicate.
- compute_queue enqueue (llm.rs:~2225-2336) UNCHANGED — still sits between Phase B and the walker per plan §4.4.
- `escalation_timeout_secs` field still on `EscalationConfig` struct (Wave 5 cleanup). Walker no longer reads it; code path that called `tokio::time::timeout(secs, pools.acquire(…))` deleted. Grep confirms only field definition + doc-comments mention it now.

---

## 2026-04-21 — commit da67787 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 1 task 11 — `pyramid_llm_audit.provider_id` schema migration + `complete_llm_audit` + `fail_llm_audit` signature extension.

**Changed:**
- `src-tauri/src/pyramid/db.rs` — idempotent `pragma_table_info` ALTER adds `provider_id TEXT` (nullable, no CHECK), mirroring the cache_hit pattern at :1038-1049. `complete_llm_audit` + `fail_llm_audit` gain a final `provider_id: Option<&str>` parameter; UPDATE statements write `provider_id = ?N`. `get_node_audit_records` + `get_llm_audit_by_id` SELECTs now project a 21st `provider_id` column; `parse_llm_audit_row` reads it into `LlmAuditRecord`. Three new tests appended to the first `tests` mod (`test_provider_id_none_legacy`, `test_provider_id_walker_style`, `test_provider_id_migration_idempotent`).
- `src-tauri/src/pyramid/llm.rs:2952,2985` — two legacy call sites updated to pass `None` (walker stamping lands in Wave 1 tasks 8-10).
- `src-tauri/src/pyramid/types.rs` — `LlmAuditRecord` gains `pub provider_id: Option<String>` with `#[serde(default)]` for backward-compat JSON.
- `src-tauri/src/main.rs` — `get_build_chronicle_events` `pyramid_llm_audit` SELECT extended to project `provider_id`; emitted in the JSON row so the chronicle UI can surface routing analytics (fleet/market/pool) alongside `model`.
- `src/components/theatre/types.ts` — TS `LlmAuditRecord` gains `provider_id?: string | null` to mirror the Rust record.

**Cargo check:** clean (default target, `cargo check` from `src-tauri/`). 70 warnings total (69 lib + 1 bin), unchanged from Wave 0 baseline.
**Cargo test:** `cargo test --lib pyramid::db::tests::test_provider_id` — 3/3 pass. Full suite `cargo test --lib` — 1758 passed, 15 failed. Delta vs Wave 0 baseline (1755 + 15): +3 new tests, zero regressions. The 15 pre-existing failures are untouched.

**Downstream-projection decisions:**
- *Extended:* Oversight/Inspector (`get_node_audit_records`, `get_llm_audit_by_id`, `LlmAuditRecord` on both sides) + chronicle (`get_build_chronicle_events`). These are the consumer paths the plan specifically calls out ("Oversight page … queries keyed on this table").
- *Deferred to Wave 5:* `src-tauri/src/pyramid/cost_model.rs::recompute_from_audit`. The current SQL groups by `(step_name, model)`; adding `provider_id` to the projection would require either (a) adding it to GROUP BY — which changes cost-model semantics and could double-bucket walker-vs-direct calls — or (b) introducing MAX(provider_id) / DISTINCT-pair aggregation, which is a design call about how routing-aware cost models should work. Plan §15 explicitly defers audit-row schema cleanup past walker; cost-model routing-aware recompute belongs in Wave 5. Behavior preserved intact for now.

**Deviation:** None.

---

## 2026-04-21 06:30 — Wave 0 wave-level verifier pass (no code changes)

**Plan task:** Wave 0 wave-level verification — fresh eyes across tasks 1-9 as an integrated whole.
**Changed:** Nothing. Audit scope covered boot-path integration, module wiring, test inventory cross-check, friction-log accuracy, cross-commit cleanup, and a dev-smoke deferral decision.

**Boot-path integration — OK.** Call ordering in `walk_bundled_contributions_manifest` (`wire_migration.rs:1317-1444`) is correct for a fresh DB: (1) `insert_bundled_contribution` loop writes the four `bundled-*dispatch_policy*` rows to `pyramid_config_contributions` (lines 1341-1359); (2) `consolidate_bundled_versions` runs; (3) `sync_chain_defaults_to_operational` + `sync_chain_assignments_to_operational` + `sync_dispatch_policy_to_operational` fire (lines 1439-1441) — the dispatch-policy sync reads the active `dispatch_policy` contribution written in step (1) and writes the operational row. Main.rs:11829 then uses `open_pyramid_connection` (pragmas only, no re-init) and `read_dispatch_policy` returns the freshly-seeded YAML → `LlmConfig.dispatch_policy` + `.provider_pools` + `compute_queue` + `fleet_roster` + `fleet_dispatch` + `compute_market_context` all populated, `tracing::info!("Dispatch policy loaded from DB — per-provider pools active, compute queue wired")` emits at line 11850. No off-by-one. Dispatcher arm at `config_contributions.rs:780` correctly routes the schema_type to `db::upsert_dispatch_policy`.

**Module integration — OK.** `pub mod compute_quote_flow;` at `pyramid/mod.rs:29`, `pub mod market_surface_cache;` at line 133. `compute_quote_flow.rs:45` imports `EntryError` + `LlmResponse` from `crate::pyramid::llm` with no circular dependency. `DispatchOrigin`, `RouteBranch`, `classify_branch`, `branch_allowed`, `EntryError` all `pub` per `llm.rs:886`/`918`/`929`/`947`/`983` — Wave 1 walker body can consume them.

**Test inventory — OK.** All 20 new Wave 0 tests present and pass:
- `cargo test --lib -- prepare_for_replay classify_branch branch_allowed entry_error compute_quote_flow market_surface_cache sync_dispatch_policy_to_operational` → 13/13 pass.
- `cargo test --lib provider_pools` → 11/11 pass (4 pre-existing + 7 new).
- Full suite `cargo test --lib` → 1755 pass / 15 fail. 15 failures match the friction-log 03:05 pre-existing set exactly — zero new regressions from Wave 0.

**Friction log — still accurate.** All four entries (03:05 pre-existing failures, 03:10 §8-vs-§2.5.1 staleness, 04:15 test-count miscount, 05:00 `/market-surface` verbatim passthrough) remain load-bearing. No new surprises from the wave-level vantage.

**Cross-commit cleanup — clean.** Grepped for inline `compute_queue = None` / `fleet_dispatch = None` / `fleet_roster = None` / `compute_market_context = None` in non-test code — only four hits, all inside `prepare_for_replay` at `llm.rs:873-876`. Every prior inline clear migrated. `resolve_uuid_from_handle` + `NetworkHandleInfo` still live in `llm.rs` and `compute_requester.rs` (rev 2.0 walker body) as expected — plan §2 schedules deletion in Wave 5. Nothing NEW in Wave 0 depends on them; `compute_quote_flow.rs` references them only in doc-comments that explicitly forbid reintroduction.

**Dev-smoke — deferred.** Wave 0 commits are all stubs or pre-walker-body helpers; no functional path to exercise end-to-end yet. Running the Tauri GUI in this sandbox is not viable (tier-"read" on browsers, tier-"click" on terminals — the dev bundle boot requires display). Per plan §8 Wave 0 success criteria ("compiles; tests pass; main.rs hydration verified by code-reading"), boot smoke is legitimately not in scope for Wave 0. Wave 1 (walker body landing) is the first functional smoke opportunity.

**Cargo check:** clean (default target). 69 lib warnings + 1 bin warning — matches pre-Wave-0 baseline.
**Cargo test:** 1755 pass / 15 fail (same pre-existing set; zero new failures).
**Deviation:** None. Wave 0 is verifier-clean; Wave 1 is unblocked.

## 2026-04-21 06:00 — commit e813720 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 8 — `compute_quote_flow` skeleton module.
**Changed:** New file `src-tauri/src/pyramid/compute_quote_flow.rs` (~266 LOC). `pub mod compute_quote_flow;` inserted between `compute_market_ops` and `compute_requester` in `src-tauri/src/pyramid/mod.rs`. Re-exports `ComputeQuoteBody`, `ComputeQuoteResponse`, `ComputeQuotePriceBreakdown`, `ComputePurchaseBody`, `ComputePurchaseResponse`, `ComputePurchaseTrigger`, `LatencyPreference` from `agent_wire_contracts` (rev `a9e356d3` — `uuid_job_id` already present on `ComputePurchaseResponse` per Q5). `ComputeFillBody` declared locally (contracts crate does not yet export it); fields match Wire-dev's Q4 answer + spec §1.8 including optional `max_tokens`. Four public stubs: `quote()`, `purchase(quote_jwt, body)`, `fill()`, `await_result()` — all `unimplemented!("Wave 3")`. Private `classify_rev21_slug()` maps all §4.2 slugs to three-tier `EntryError` with per-arm rationale doc-comments; unknown-slug default is `RouteSkipped` (conservative advance). **No `resolve_uuid_from_purchase`** — rev-2.1 `/purchase` response carries `uuid_job_id` directly. Module-doc banner forbids reintroduction.
**Cargo check:** clean (default target). 69 lib warnings (same baseline).
**Cargo test:** `cargo test --lib compute_quote_flow` — 1/1 pass (`classify_rev21_slug_maps_insufficient_balance_to_route_skipped`).
**Deviation:** None. Bodies pending Wave 3 per plan §8 task 8 ("stubs returning `unimplemented!(\"Wave 3\")` — body goes in Wave 3").

## 2026-04-21 05:00 — commit 80c962a (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 9 — `MarketSurfaceCache` skeleton module.
**Changed:** New file `src-tauri/src/pyramid/market_surface_cache.rs` (~120 LOC). `pub mod market_surface_cache;` inserted alphabetically between `market_mirror` and `pending_jobs` in `src-tauri/src/pyramid/mod.rs`. Types: local `CacheData { market: MarketSurfaceMarket, models: HashMap<String, MarketSurfaceModel>, generated_at: DateTime<Utc> }` + `MarketSurfaceCache { data: Arc<RwLock<Option<CacheData>>>, last_refresh_at: Arc<RwLock<Instant>> }`. Methods: `new()` (live), `get_model(model_id)` (live read path — returns `None` on cold cache), `refresh_now()` (`unimplemented!("Wave 4")`), `spawn_poller(auth, config, cache)` (logs stub + returns — Wave 4 replaces body). `MarketSurfaceMarket` and `MarketSurfaceModel` reused from `agent-wire-contracts` rev `a9e356d3`; no local type declarations needed for the Wire-side schema. The `HashMap`-indexed `models` field diverges from the contracts crate's `Vec<MarketSurfaceModel>` — walker needs O(1) lookup, so Wave 4 poller will index on refresh.
**Cargo check:** clean (default target). 69 lib warnings (below existing 70 baseline — `#[allow(dead_code)]` on `last_refresh_at` until Wave 4 wires it).
**Cargo test:** `cargo test --lib market_surface_cache` — 1/1 pass (`cold_cache_get_model_returns_none`).
**Deviation:** None structurally. Spec §6.1 shows `last_refresh_at: Arc<RwLock<Instant>>` alongside `data`; kept verbatim even though Wave 0 doesn't touch it, so Wave 4 poller doesn't need a struct-shape change. `Default` impl added (trivial) for ergonomics.

## 2026-04-21 04:30 — commit f88dec3 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 tasks 5 + 6 — `RouteBranch` + `classify_branch` + `branch_allowed` + `EntryError` taxonomy.
**Changed:** `src-tauri/src/pyramid/llm.rs`, inserted right after the `DispatchOrigin` impl block (plan §2.5.2 + §2.5.3):
  - `pub enum RouteBranch { Fleet, Market, Pool }` with `Debug/Clone/Copy/PartialEq/Eq` derives.
  - `pub fn classify_branch(provider_id: &str) -> RouteBranch` — maps `"fleet"` / `"market"` sentinels to the walker branches; everything else is `Pool`.
  - `pub fn branch_allowed(branch: RouteBranch, origin: DispatchOrigin) -> bool` — Pool always allowed; Fleet + Market allowed only for `Local` origin per the "inbound jobs don't re-dispatch" invariant.
  - `pub enum EntryError { Retryable { reason }, RouteSkipped { reason }, CallTerminal { reason } }` with `Debug` derive plus `Display` + `std::error::Error` impls and `variant_tag()` + `reason()` accessors. Doc-comments pin the walker semantic: first two advance, third bubbles to caller.
  - 7 new unit tests in `mod tests`: `classify_branch_maps_sentinels_to_walker_branches`, `branch_allowed_pool_always_ok`, `branch_allowed_fleet_only_from_local`, `branch_allowed_market_only_from_local`, `entry_error_variant_tags_match_chronicle_vocab`, `entry_error_reason_accessor_uniform_across_variants`, `entry_error_display_matches_variant_tag_colon_reason`. Together they cover all 3×3 branch×origin pairs and all three EntryError variants.
  - No call sites yet — walker body in Wave 1 consumes these.
**Cargo check:** clean (default target). No new warnings.
**Cargo test:** `cargo test --lib -- classify_branch branch_allowed entry_error` — 7/7 pass.
**Deviation:** Added two small ergonomic methods to `EntryError` beyond the plan's bare enum: `variant_tag()` (short chronicle tag) + `reason()` (uniform accessor) + a `Display` impl. Not structural — the walker in Wave 1 will consume both. Trivially reversible if they prove unused.

## 2026-04-21 03:30 — commit b3777d6 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 7 — `ProviderPools::try_acquire_owned` + `SlidingWindowLimiter::try_acquire` non-blocking helpers.
**Changed:** `src-tauri/src/pyramid/provider_pools.rs`:
  - New `AcquireError` enum with `Unavailable(String)` + `Saturated` variants plus `Display` + `std::error::Error` impls. Walker discriminates these for chronicle labeling (`network_route_unavailable` vs `network_route_saturated`, plan §4.3).
  - New `SlidingWindowLimiter::try_acquire(&self) -> bool` — uses `TokioMutex::try_lock()` to stay sync; contention treated as conservative saturation. `max_requests == 0` (disabled) always returns true. Mirrors the eviction logic of `wait()` but never sleeps.
  - New `ProviderPools::try_acquire_owned(&self, provider_id: &str) -> Result<OwnedSemaphorePermit, AcquireError>` — rate-limiter check first (cheaper); `semaphore.clone().try_acquire_owned()` second; both failures map to `Saturated`, unknown provider maps to `Unavailable("provider_not_in_pool")`.
  - 7 new unit tests covering: known-provider ok, unknown-provider unavailable, semaphore-exhausted saturated, rate-limiter-full saturated, limiter under-limit accepts 3/3, limiter at-limit refuses 3rd, limiter disabled (max_requests=0) always accepts.
**Cargo check:** clean (default target). No new warnings.
**Cargo test:** `cargo test --lib provider_pools` — 11/11 pass (3 pre-existing + 8 new).
**Deviation:** Plan §7 pseudocode used `Result<OwnedSemaphorePermit, AcquireError>` (bare `Result`) — used fully-qualified `std::result::Result` in the impl signature to avoid shadowing issues against the module's `anyhow::Result` import. Semantic identical.

## 2026-04-21 04:15 — verifier pass (no code changes)

**Plan task:** Wave 0 task 7 serial verifier — `ProviderPools::try_acquire_owned` + `SlidingWindowLimiter::try_acquire`.
**Changed:** Nothing. Audit confirmed: `AcquireError` exposes exactly the two variants walker needs (`Unavailable(String)` + `Saturated`) with `Display` + `std::error::Error` impls; `try_acquire_owned` runs rate-limiter check before semaphore CAS (cheaper-first, correct); `SlidingWindowLimiter::try_acquire` evicts expired entries before the capacity check (lines 125-131); lock-contention → false is conservative-saturation and matches the walker's cheap-advance contract; `max_requests == 0` early-return mirrors `wait()`'s disable-semantic; signature uses fully-qualified `std::result::Result` to sidestep the module-top `anyhow::Result` import; test coverage hits all three limiter states (under-limit, at-limit, disabled) and all four acquire outcomes (Ok, Unavailable, Saturated-semaphore, Saturated-rate-limiter); no lingering `.acquire()` sites require migration per plan §7 ("existing acquire stays").
**Cargo check:** clean (default target). 70 pre-existing warnings unchanged.
**Cargo test:** `cargo test --lib provider_pools` — 11/11 pass. `cargo test --lib` — 1746 passed / 15 failed, matching the known pre-existing friction-log set exactly (no new failures from this commit).
**Deviation:** Prior impl-log entry at 03:40 reports "3 pre-existing + 8 new" tests; actual count is 4 pre-existing + 7 new = 11 total. Commit message carries the same miscount. No functional impact — 11/11 pass either way. Noted in friction log.

## 2026-04-21 03:20 — verifier pass (no code changes)

**Plan task:** Wave 0 task 4 serial verifier.
**Changed:** Nothing. Audit confirmed: `prepare_for_replay` matches §2.5.1 contract; all four call sites correctly migrated; tests cover all three origin variants with durable-field preservation; no lingering hand-clears; `DispatchOrigin` has `Copy` so the `options.dispatch_origin` pass at llm.rs:2126 works without explicit clone; §8-vs-§2.5.1 doc-rot acknowledged and deferred to Wave 5 doc-sweep.
**Cargo check:** clean (default target).
**Cargo test:** `cargo test --lib prepare_for_replay` — 3/3 pass.
**Deviation:** None.

## 2026-04-21 03:10 — commit f0ebeb0 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 4 — `LlmConfig::prepare_for_replay(origin)` helper + 4 call sites.
**Changed:** Added `prepare_for_replay` method to `impl LlmConfig` at `src-tauri/src/pyramid/llm.rs:854-879` — clears `compute_queue`, `fleet_dispatch`, `fleet_roster`, `compute_market_context` (origin-independent), emits `tracing::debug!` with origin for observability. Updated all four call sites:
  - `src-tauri/src/pyramid/llm.rs:2099` (Local/queue-enqueue replay; was missing compute_market_context clear — latent bug now closed).
  - `src-tauri/src/server.rs:2030` (FleetReceived inbound worker; was missing compute_queue + compute_market_context — latent bug closed).
  - `src-tauri/src/server.rs:3958` (MarketReceived inbound worker; was missing compute_queue + compute_market_context — latent bug closed).
  - `src-tauri/src/pyramid/dadbear_supervisor.rs:548` (DADBEAR queue entry; was only clearing compute_queue — latent bug closed).
Added three unit tests in `llm.rs` `mod tests`: `prepare_for_replay_local_clears_all_dispatch_handles`, `prepare_for_replay_fleet_received_clears_all_dispatch_handles`, `prepare_for_replay_market_received_clears_all_dispatch_handles`. Shared fixture `build_live_config_with_all_dispatch_handles_for_test` populates all six runtime handles (including `compute_market_context`, which the pre-existing `with_runtime_overlays` fixture omits).
**Cargo check:** clean (default target). Same 69+1 pre-existing warnings, no new warnings from task 4.
**Cargo test:** `cargo test --lib prepare_for_replay` — 3/3 pass. Full-suite `cargo test --lib` — 1739 pass / 15 pre-existing fail (friction log 2026-04-21 03:05 entry confirms all 15 also fail on main WITHOUT my changes; NOT caused by task 4).
**Deviation:** Plan §8 Wave 0 task 4 test-description says "Local-origin only clears compute_queue" — this contradicts plan §2.5.1's explicit "origin-independent by design: clear all four unconditionally." Implemented per §2.5.1 (newer + detailed authority). Friction-log entry filed flagging §8 text as stale relative to §2.5.1.

## 2026-04-21 02:55 — no commit (task 3 verification-only)

**Plan task:** Wave 0 task 3 — verify main.rs boot hydration reads dispatch_policy.
**Changed:** Nothing. Verified `src-tauri/src/main.rs:11829-11887` already:
  - Opens the pyramid DB connection.
  - Calls `db::read_dispatch_policy` to fetch YAML.
  - Parses into `DispatchPolicyYaml` → `DispatchPolicy::from_yaml`.
  - Builds `ProviderPools::new(&policy)`.
  - Writes `LlmConfig.dispatch_policy` + `LlmConfig.provider_pools` + `compute_queue` + `fleet_roster` + `fleet_dispatch` + `compute_market_context`.
  - Emits `tracing::info!("Dispatch policy loaded from DB — per-provider pools active, compute queue wired")` at line 11850.
  - ConfigSynced listener at 11889+ rebuilds on contribution update (AD-8 Part 1 comment).
**Cargo check:** N/A.
**Cargo test:** N/A.
**Deviation:** None.

## 2026-04-21 — commit eda0dde (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 2 — `sync_dispatch_policy_to_operational` helper in `wire_migration.rs`.
**Changed:** Added `sync_dispatch_policy_to_operational(conn)` at `src-tauri/src/pyramid/wire_migration.rs` right after `sync_chain_assignments_to_operational`, mirroring `sync_chain_defaults_to_operational` (schema_type = `dispatch_policy`, status = `active`, ORDER BY accepted_at DESC LIMIT 1). Parses YAML into `dispatch_policy::DispatchPolicyYaml` for validation (surfaces malformed YAML at boot) then calls `db::upsert_dispatch_policy(conn, &None, &yaml_content, &contribution_id)` — the operational table stores raw YAML for hot-reload, parsed struct is discarded. Wired peer call into `walk_bundled_contributions_manifest` alongside `sync_chain_defaults_to_operational` + `sync_chain_assignments_to_operational` (line 1441). Added one in-module test `sync_dispatch_policy_to_operational_hydrates_row` using the existing `insert_active_row` + `mem_conn` fixtures: inserts an active `dispatch_policy` contribution, runs the helper, asserts the operational row holds the YAML and contribution_id.
**Cargo check:** clean (default target — `cargo check` from `src-tauri/`). Only pre-existing dead-code / deprecated-API warnings, all unrelated to this change.
**Cargo test:** `cargo test --lib wire_migration` — 27/27 pass. New test `sync_dispatch_policy_to_operational_hydrates_row` included.
**Deviation:** None. Plan §8 Wave 0 task 2 says "parses YAML → calls db::upsert_dispatch_policy"; `upsert_dispatch_policy` takes raw YAML string, so parsing is validation-only (same information-preserving pattern as the chain-defaults mirror, which parses to `ChainDefaultsYaml` then passes mappings — here the operational table takes raw YAML, so the parse surfaces errors at boot and the raw string is handed through).

## 2026-04-21 02:30 — commit e18261d (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 task 1 — bundle `dispatch_policy-default-v1` contribution family (4 entries).
**Changed:** `src-tauri/assets/bundled_contributions.json` gains four entries directly after the `evidence_policy` family: `bundled-skill-generation-dispatch_policy-v1` (intent→YAML generation prompt), `bundled-schema_definition-dispatch_policy-v1` (JSON Schema covering version / provider_pools / routing_rules — including optional `max_budget_credits` on `RouteEntry` per plan §2 — plus escalation / build_coordination / max_batch_cost_usd / max_daily_cost_usd), `bundled-schema_annotation-dispatch_policy-v1` (Tools wizard field annotations), and `bundled-dispatch_policy-default-v1` (default seed with `market → fleet → openrouter → ollama-local` chain, seed YAML verbatim from plan §8 Wave 0 task 1). Default seed deliberately omits `max_budget_credits` so absent → None → NO_BUDGET_CAP sentinel at read time. Schema-definition JSON round-trips through `jq .` and `json.loads`; seed YAML round-trips through `jq -r` with correct provider chain. Dispatcher arm at `config_contributions.rs:780` already routes `dispatch_policy` contributions to `db::upsert_dispatch_policy`, so the seed lands in the operational table via the existing path.
**Cargo check:** N/A (JSON-only change).
**Cargo test:** N/A (JSON-only change). `jq . src-tauri/assets/bundled_contributions.json > /dev/null` exits 0.
**Deviation:** None.

## 2026-04-21 02:05 — commit 3d20232 (branch walker-re-plan-wire-2.1)

**Plan task:** Wave 0 prereq — contracts bump for Q5 + Q6.
**Changed:** `src-tauri/Cargo.toml:31` bumps agent-wire-contracts from `1adb3f20` → `a9e356d3`. Cargo.lock updated. Picks up Q5 `uuid_job_id` on `/purchase` 200 response + Q6 `/match` 410 Sunset header corrected to 2026-05-31.
**Cargo check:** clean (default target). 70 pre-existing warnings unchanged (dead code on `WorkItem`/`InFlightItem` fields, deprecated `tauri_plugin_shell::Shell::open` call at main.rs:5797). No new warnings from contract bump.
**Cargo test:** not run (no code change).
**Deviation:** None. Wire-dev commit `a9e356d3` landed before Wave 0 implementation started, so walker Wave 3 can use the direct `uuid_job_id` path from the purchase response without the fallback poll. Fallback path still implemented as belt-and-suspenders per plan §9 Q5 resolution.

## 2026-04-21 01:30 — commit 523195c (branch walker-re-plan-wire-2.1)

**Plan task:** Pre-Wave-0 — absorb planner Q&A (15 answers) + Wire-dev Q1-Q7 resolutions.
**Changed:** Plan §2.5.1 snippet updated to use named `origin` param with `tracing::debug!` emit. Plan Wave 1 task 11 extended to spell out three walker exit outcomes (Success / CallTerminal / Exhaustion) and cover BOTH `complete_llm_audit` and `fail_llm_audit` signature extension. Handoff appended with full Q&A section, 19-entry chronicle event constants block, Wave 3 parallelism split (3a/3b/3c), overnight dev-smoke protocol, and small-work direct-write pattern for Wave 0 tasks 4/5/6/7.
**Cargo check:** not run (docs only).
**Cargo test:** not run (docs only).
**Deviation:** None. Absorbs Adam's 15 planner answers and Wire guy's 7-question response; Q4 unblocked (input_token_count + privacy_tier still honored in /fill).

## 2026-04-21 01:10 — commit 5530881 (branch walker-re-plan-wire-2.1)

**Plan task:** Pre-Wave-0 — seed implementation + friction logs.
**Changed:** Created `docs/plans/walker-re-plan-wire-2.1-IMPL-LOG.md` and `docs/plans/walker-re-plan-wire-2.1-FRICTION-LOG.md` with templates per handoff "log templates" section. Branch `walker-re-plan-wire-2.1` cut from main at `f6ce69c`.
**Cargo check:** not run (docs only).
**Cargo test:** not run (docs only).
**Deviation:** None.

## 2026-04-21 01:05 — commit f6ce69c (branch main)

**Plan task:** Pre-branch checkpoint — commit plan rev 0.3 + handoff on main, push to github.
**Changed:** Added `docs/plans/walker-re-plan-wire-2.1.md` (rev 0.3, 902 lines) and `docs/plans/walker-re-plan-wire-2.1-HANDOFF.md` (320 lines).
**Cargo check:** not run (docs only).
**Cargo test:** not run (docs only).
**Deviation:** None.
