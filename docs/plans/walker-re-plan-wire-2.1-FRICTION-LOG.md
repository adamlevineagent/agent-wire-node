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
