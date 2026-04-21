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
