# Walker Re-Plan Wire 2.1 — Friction Log

Real-time record of surprises, workarounds, and "this bit me" moments. Newest at top.

**Plan:** `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3
**Handoff:** `docs/plans/walker-re-plan-wire-2.1-HANDOFF.md`
**Branch:** `walker-re-plan-wire-2.1`

---

<!--
Entry template:

## <YYYY-MM-DD HH:MM> — <task or file:line context>

**Context:** <what you were doing>
**Surprise:** <what bit you>
**Root cause:** <if known, else "still investigating">
**Workaround:** <how you got unstuck, or "blocked awaiting X">
**Flag:** plan error / doc staleness / spec ambiguity / Wire-side bug / learning moment
-->

## 2026-04-21 — Wave 3a divergences (agent findings)

**Context:** Parallel agents for compute_quote_flow bodies (3a-A) + market_surface_cache polling (3a-B) shipped cleanly. Both flagged plan/contract/spec drift worth surfacing for Wave 3b and Wire-dev.

**3a-A (compute_quote_flow) findings — 5 divergences:**

1. **Spec §2.2 missing `uuid_job_id` in response shape.** Wire-dev commit `a9e356d3` shipped Q5 (added to contracts), but the spec doc still shows `{job_id, request_id, dispatch_deadline_at}` without uuid_job_id. Walker code uses `purchase_response.uuid_job_id` directly (correct). **Action:** Wire-dev should sync the spec doc to match contracts.

2. **`X-Quote-Token` header on /purchase — divergence between prompt and spec §2.2.** Spec keeps `quote_jwt` exclusively in the body. Agent followed spec (no header). Walker code OK. **Action:** none unless Wire-side ever wants the header.

3. **Tier classifications: plan §4.2 says `Retryable` for `platform_unavailable` + `economic_parameter_missing`; prompt said `RouteSkipped` + `CallTerminal`.** Agent followed prompt. Rationale: walker v1 advances rather than sleeps on transient platform outages (RouteSkipped), and operator-level config-missing is walk-bug-category (CallTerminal bubbles). Plan §4.2 could be reconciled in Wave 5 doc sweep; current behavior is more walker-friendly.

4. **`purchase()` signature param order (plan §4.2 vs skeleton).** Skeleton/prompt: `purchase(auth, config, quote_jwt, body)`; plan §4.2: `purchase(purchase_body, auth, config)` with quote_jwt in body. Implementation honors prompt signature; separate `quote_jwt` param overwrites `body.quote_jwt` before send. Harmless divergence.

5. **🔴 PendingJobs registration timing — race hazard.** Plan §4.2 registers the oneshot BEFORE `/fill` (race-safe: provider can only deliver after we've registered). Prompt directed registration inside `await_result` which runs AFTER `/fill`. Agent followed prompt + added a doc-comment flagging the race. **Action — Wave 3b (walker market-branch inline) MUST fix this:** walker calls `purchase`, gets `uuid_job_id`, registers the oneshot with PendingJobs, THEN calls `fill`. `await_result` should take an already-registered `oneshot::Receiver` rather than registering internally. Alternatively: expose a `pub fn register(pending_jobs, uuid) -> Receiver` helper in compute_quote_flow and have walker call it between purchase and fill. This is a correctness bug that must not ship.

**3a-B (market_surface_cache) findings — 1 divergence:**

6. **Plan §6.1 `CacheData` shape vs contracts crate `MarketSurfaceResponse`.** Plan declares `CacheData { market, models, generated_at: DateTime<Utc> }`. Contracts crate has `market`, `models`, `catalog` — NO top-level `generated_at`. Agent used `market.last_updated_at: String` parsed as RFC-3339 (fallback `Utc::now()` on parse failure). Also multiple field-name divergences in nested types (FloatPool/Queue/Performance/Demand24h) — plan §6 prose has an older schema; tests written against real contracts shapes. **Action:** Wave 4/5 doc sweep should re-sync plan §6 prose with contracts crate reality.

## 2026-04-21 — Wave 2 retro

**What worked:**
- Single workflow agent cleanly handled the full Phase A extraction + walker fleet branch + delete + resolve_local filter update in one session. LOC moved was ~491 (bigger than Wave 1's Phase D replacement) but the extraction-into-helper pattern (`dispatch_fleet_entry` + `FleetDispatchArgs`) was a structural fit — no improvisation needed.
- Plan §4.1 error-classification table was load-bearing: agent consulted it directly for every fleet failure mode (timeout → Retryable, orphaned → Retryable, POST failures → RouteSkipped, etc.). No CallTerminal tier from fleet; walker's defensive CallTerminal arm stayed for symmetry with pool.
- Verifier came back zero-find for the second wave running. The workflow-agent → verifier cadence is proving reliable for well-scoped surgery.

**What bit us:**
- Nothing significant. The wave1→wave2 stub rename (entry count 2→3 because fleet now walks instead of being pre-filtered) was the only cross-wave test touch. Learning confirmed: test names should describe behavior, not wave numbers. Wave 3 will need a third rename (wave2→wave3) when market joins; consider renaming to something stable like `walker_skips_inline_stubbed_branches` if this pattern repeats.

**What we'd do differently:**
- Wave 3 splits per Adam's answer #8 into 3a (parallel new-file bodies) + 3b (serial walker market inline) + 3c (tests + wanderer). Respect the split; don't try to one-shot Wave 3.

## 2026-04-21 — Wave 1 retro

**What worked:**
- Serializing task 11 (sig extension) before the walker-body agent meant the walker consumer hit stable `complete_llm_audit` / `fail_llm_audit` signatures without a rebase. Workflow agent shipped 4 atomic commits (constants / refactor / tests / impl-log) — easy to review.
- Walker-body agent's `'http: { loop { ... } }` named-block pattern for HTTP retry carry-over turned out clean. `break 'http Err(EntryError::...)` reads as a state machine.
- Wanderer after verifier caught nothing new — which is actually the signal. Verifier found the two tightening fixes (kill no-op, kill dead helper); wanderer confirmed end-to-end wiring. When wanderers come back empty, the per-task + verifier gates did their job.

**What bit us:**
- Initial walker-body agent shipped a `let _ = (&mut provider_impl, &mut secret, &mut provider_type);` warning-silencer. Verifier traced the warning to `mut` qualifiers that were Phase D vestiges — drop `mut`, warnings die. Learning: when an agent leaves an ugly no-op, it's often a missed refactor upstream. Default to understanding the warning source before silencing.
- `maybe_fail_audit` helper got `#[allow(dead_code)]`-tagged with a "reuse in Wave 2-3" note, but verifier correctly killed it: Waves 2-3 inline fleet+market INTO the walker where `fail_llm_audit` is already called inline with `provider_id`. Ambiguous deferrals become permanent dead code. Learning: either reuse path is concrete or the helper dies — don't defer without a specific call site name.
- Dev-smoke deferred to Wave 4 gate per plan. Adam's running binary on 8765 is a different branch; clobbering would lose his dev state. Cargo-green + wanderer is Wave-1 ship signal.

**What we'd do differently:**
- Wave 2 extracts Phase A fleet into the walker. The walker-body agent already had to reason about Phase A/B staying untouched; Wave 2 removes one of those constraints. Keep the same "workflow agent → serial verifier → wanderer" cadence; Wave 2 LOC target is 400 and localized, so same shape as Wave 1.
- If a wave's agent tags anything `#[allow(dead_code)]` or `#[allow(unused_*)]`, ensure the comment names a specific follow-up task + why. Verifier should reject any allow without both.

## 2026-04-21 — Wave 1 verifier pass — hoisted provider bindings are genuine fallback

**Context:** Verifier inspecting the Wave 1 walker-body diff; agent had written `let _ = (&mut provider_impl, &mut secret, &mut provider_type);` at llm.rs:2443 as a warning-silencer.
**Surprise:** The ugly no-op read "smells" like dead code waiting to be deleted, but grep showed `provider_type` is still used by `should_enqueue_local_execution` (llm.rs:871) in the queue-enqueue gate, AND `_provider_impl` / `_secret` are the fallback values the walker's pool branch reaches for when `config.provider_registry` is absent (tests / pre-init). So the destructure as a whole can't be deleted.
**Root cause:** Waves 2-3 remove Phase A / Phase B (which don't read these either); once Wave 5 deprecates the `resolved_route = None` fallback path the outer destructure is deletable entirely. Today they're pure pre-init scaffolding.
**Workaround:** Drop `mut` qualifiers (never reassigned), prefix unused bindings with `_`, drop the no-op. Warnings die, code reads honestly. Full deletion deferred to Wave 5 cleanup.
**Flag:** learning moment — `#[allow(unused_assignments)]` on `last_attempted_provider_id` is the other "is this needed" case: verified by removing it and watching the warning fire (walker can exhaust without any pool attempt, leaving the write unread). Allow stays; comment updated to explain WHY.

## 2026-04-21 05:20 — Wave 0 retro

**What worked:**
- Parallel background agents on disjoint files (task 1 JSON + task 2 Rust, later tasks 8+9 NEW files) cut wall time ~40% vs serial without causing git races. Task 9 agent's stash-dance when it hit task 8's in-progress `pub mod` line is the shape that worked.
- Small-work direct-write + serial verifier (tasks 4, 5/6, 7) produced clean code first time; verifiers caught ZERO structural bugs. The pattern is load-bearing — drop it and we'd need longer workflow-agent prompts + more integration re-audits.
- Pre-reading plan §2.5.1 / §2.5.2 / §2.5.3 before writing helpers kept implementations tight — no speculative extras.

**What bit us:**
- §8 vs §2.5.1 contradiction on `prepare_for_replay` origin semantics — rev 0.3 absorbed §2.5.1 but didn't back-sync §8 task-4 test description. Cost: one friction entry + Deviation note. Deferred to Wave 5 doc sweep. Learning: when rev-bumping a plan, grep §8 task lists for text mirroring the changed §.
- Test-count miscount in task 7 commit (friction 04:15). Zero functional impact. Learning: narrate test deltas from the file, not memory.
- 15 pre-existing `cargo test --lib` failures on main block "full-suite clean" as a phase-complete signal. Spawned separate task via `mcp__ccd_session__spawn_task`. Not a walker issue.

**What we'd do differently:**
- Wave 0 was all prereqs — minimal end-to-end surface to dev-smoke. Wave 1's walker shell produces the first user-visible path; queue dev-smoke for that. Don't invest in Wave 0 GUI smoke.
- When a workflow agent hits another concurrent agent's uncommitted file (task 9's mod.rs collision with task 8), use the stash-commit-pop recipe observed. Document it in the handoff if it recurs.

## 2026-04-21 05:00 — Wave 0 task 9 — `/market-surface` already has a verbatim-passthrough IPC

**Context:** Authoring `MarketSurfaceCache` skeleton (plan §6 / Wave 0 task 9).
**Surprise:** `compute_market_ops::market_surface` (`src-tauri/src/pyramid/compute_market_ops.rs:498-518`) already calls `/api/v1/compute/market-surface` and returns the raw `serde_json::Value` to whichever IPC consumer hit it. Separate from the cache; doesn't parse into the contracts crate types.
**Root cause:** Pre-rev-2.1 Settings/compute-market UI wired a direct fetch through the ops layer. Pre-dates the rev 2.1 `MarketSurfaceResponse` contracts types landing.
**Workaround:** Leave it alone for Wave 0. Wave 4 should decide: (a) keep it as a verbatim escape hatch for UI that wants the raw JSON, or (b) route it through `MarketSurfaceCache::get_cached_json()` so UI reads the same snapshot walker does and we don't double-poll. Prefer (b) — flagged here so Wave 4 doesn't re-discover independently.
**Flag:** learning moment (not plan error; §6.4 already hints at "cache consumers are walker + Settings panel" which is the right endpoint).

## 2026-04-21 04:15 — Wave 0 task 7 — test-count miscount in commit message + impl-log entry

**Context:** Serial-verifier audit of `ProviderPools::try_acquire_owned` at commit b3777d6.
**Surprise:** Commit message and prior impl-log entry both say "8 new unit tests" / "3 pre-existing + 8 new" — actual file has 4 pre-existing tests (`test_pools_created_from_policy`, `test_sequencer_created_for_sequential_rules`, `test_acquire_known_provider`, `test_acquire_unknown_provider_errors`) and 7 new tests (3 for `SlidingWindowLimiter::try_acquire` + 4 for `ProviderPools::try_acquire_owned`). Total matches the 11/11 passing run either way.
**Root cause:** Off-by-one in narrative authorship; not a code issue. The `_errors` pre-existing test likely uncounted when narrator wrote "3 pre-existing".
**Workaround:** Left commit history untouched (fix would be amend, violating "create new commits" rule). Flagged in verifier impl-log entry so downstream readers aren't confused.
**Flag:** learning moment — narrate test deltas by reading the file, not by counting your own additions.

## 2026-04-21 03:10 — Wave 0 task 4 — plan §8 vs §2.5.1 contradiction on prepare_for_replay origin behavior

**Context:** Implementing `LlmConfig::prepare_for_replay(origin)` per plan §2.5.1 + §8 Wave 0 task 4.
**Surprise:** The two sections describe different origin semantics:
  - **§2.5.1 (code snippet + rationale):** origin-independent; all four fields (`compute_queue`, `fleet_dispatch`, `fleet_roster`, `compute_market_context`) cleared unconditionally. Rationale: "whenever `prepare_for_replay` is called, the OUTER dispatch decision has already been made. The inner (replayed) call should be pool-only."
  - **§8 Wave 0 task 4 test spec:** "assert Local-origin only clears `compute_queue`." Implies Local clears ONE field while FleetReceived/MarketReceived clear THREE.
**Root cause:** §8 task-4 test description predates rev 0.3's §2.5.1 consolidation. Adam's answer #12 (2026-04-21) explicitly endorsed the §2.5.1 shape (tracing::debug + unconditional 4-field clear) without flagging §8 as needing a sync edit. The §8 stale text survived the rev 0.3 absorption.
**Workaround:** Implemented per §2.5.1 (authoritative, consolidated). Tests assert all four fields cleared for all three origins. §8 task-4 line wasn't updated in this commit — will get swept with the Wave 5 cleanup pass or fix inline if I touch §8 for another reason in a later wave.
**Flag:** plan staleness — §8 task-4 test-description string vs §2.5.1 spec. Not blocking; behavior correctness is covered by §2.5.1.

## 2026-04-21 03:05 — pre-existing test failures on walker branch base

**Context:** Ran full-suite `cargo test --lib` after task 4 changes; expected to see baseline behavior.
**Surprise:** 15 tests fail. Sampled `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence` — asserts evidence rows coexist across slugs, fails with `assertion left == right: 0 vs 2`. Not an LLM / walker concern.
**Root cause:** Verified pre-existing — `git stash -- src/pyramid/llm.rs src/server.rs src/pyramid/dadbear_supervisor.rs` and rerunning reproduces the exact same failure. Failures are in db / staleness / schema_registry / yaml_renderer / defaults_adapter / question_compiler / provider_registry — all broad-cross-area. Likely a shared-fixture regression from a recent merge that wasn't caught because no one runs full `cargo test --lib` on main.
**Workaround:** Proceeding with walker work; flagged as separate concern. Spawning a side task via `mcp__ccd_session__spawn_task` is warranted per Adam's feedback #10 ("exception: if it's a BIG bug ... friction-log + spawn separate task"). 15 failures across unrelated subsystems is BIG — it would balloon Wave 0 to investigate here.
**Flag:** pre-existing bug (not plan-related). Action: spawn-task chip.

**List of pre-existing failures (captured for side-task handoff):**
- `pyramid::db::provider_registry_tests::init_seeds_four_tiers_but_not_stale_local`
- `pyramid::db::tests::test_evidence_pk_cross_slug_coexistence`
- `pyramid::defaults_adapter::tests::real_yaml_thread_clustering_preserves_response_schema`
- `pyramid::question_compiler::tests::code_yaml_l0_webbing_uses_correct_model`
- `pyramid::schema_registry::tests::test_hydrate_from_bundled_manifest`
- `pyramid::staleness::tests::test_below_threshold_not_enqueued`
- `pyramid::staleness::tests::test_deletion_skips_first_attenuation`
- `pyramid::staleness::tests::test_path_normalization`
- `pyramid::staleness::tests::test_propagate_staleness_with_db`
- `pyramid::staleness::tests::test_shared_node_higher_score_propagates`
- `pyramid::yaml_renderer::tests::test_resolve_options_tier_registry_seeded`
- (4 more printed in failure list — retrievable via `cargo test --lib 2>&1 | grep FAILED`)
