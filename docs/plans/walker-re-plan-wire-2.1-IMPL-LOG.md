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
