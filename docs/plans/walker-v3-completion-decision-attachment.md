# Walker v3 Completion — Decision Attachment Closure

**Date:** 2026-04-23
**Status:** APPROVED FOR IMPLEMENTATION — Adam green-lit Lane B (Promote) with specific tier renames + model picks on 2026-04-23. Wave 0 verification pending.
**Rev:** 1.0
**Branch:** `walker-v3-shipping`
**Supersedes:** nothing. NEW scope not owed by walker v3.

**Author context:** Post-compact fork session. Walker v3 phases 0a/0b/1 (W1a–W4), 2, 3, 4, 5 shipped on this branch. This plan closes the Pattern-4 Decision-attachment bypass that past-me shipped knowingly as pragmatic (W2c handoff recommended `PyramidState::make_synthetic_step_ctx_with_decision`; helper was never built but the slot-aware constructor `make_step_ctx_from_llm_config_for_slot_and_model` serves the same purpose and already exists).

**Prereq reads:**
- `docs/plans/walker-provider-configs-and-slot-policy-v3.md` rev 1.0.2 §2.9 (Decision spine), §2.6 (provider readiness), §2.8 (tier names are arbitrary strings)
- `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3 §2.5 (systemic helpers)
- `docs/plans/walker-re-plan-wire-2.1-IMPL-LOG.md` — W2, W3a, W3b, W3c commit messages (bypass sites migrated, deferred cleanup flagged)
- W3c commit `64a9cde` — "THE field deletion + totality sweep" (22 maintenance sites migrated to `_with_model` + `model_override`)
- W1b commit `ffe32b0` — Decision attachment at chain_dispatch outer-step entry

---

## 1. One-paragraph statement

Walker v3's §2.9 Decision spine covers chain-driven LLM dispatch via clone-propagation from `chain_dispatch::dispatch_llm`/`dispatch_ir_llm`. Roughly 30 Pattern-4 sites outside that path — evidence_answering, stale_helpers, stale_helpers_upper, faq, delta, meta, webbing, supersession, extraction_schema, vine, routes, reroll, migration_config, generative_config — build StepContexts that never attach a Decision. Three bypass mechanisms coexist today: (1) callers pass `ctx = None` to the LLM call, (2) callers build a manual `StepContext::new` with `with_model_resolution` but skip `with_dispatch_decision_if_available`, (3) callers use the W3c workaround `make_step_ctx_from_llm_config_with_model` which hardcodes slot="primary" so Decision-attach early-returns. Effect: Market/Fleet/Ollama-local branches are skipped silently (`walker_v3_no_model` log + continue), dispatch falls through to OpenRouter via `LlmCallOptions.model_override` when present, or fails outright when not. This plan closes every gap. Canonical API collapses three constructors into one slot-requiring helper. Every non-test LLM dispatch site uses it. Runtime guard at walker entry makes silent bypass impossible. Regression fence test prevents reintroduction.

---

## 2. Decision 1 — tier mapping (COMMITTED)

Adam's picks on 2026-04-23, Lane B (Promote to first-class walker tiers). Rust slot literals renamed to match their canonical walker tier names.

| Rust slot (before) | Rust slot (after) | Model | Context limit | Callers |
|---|---|---|---|---|
| `fast_extract` | `evidence_loop` | `xiaomi/mimo-v2.5-pro` | 1_000_000 | evidence_answering × 4 |
| `stale_local` | `stale_l0` | `minimax/minimax-m2.7` | 197_000 | stale_helpers × 4 |
| `stale_remote` | `stale_upper` | `xiaomi/mimo-v2.5-pro` | 1_000_000 | stale_helpers_upper × 6 |
| `reroll` | `reroll` | `xiaomi/mimo-v2.5-pro` | 1_000_000 | reroll.rs × 1 |

These four tiers get first-class entries in the `walker_provider_openrouter` bundled seed with explicit `context_limit`. Existing six tiers (max / high / synth_heavy / mid / web / extractor) unchanged.

Additional slot assignments for other Pattern-4 sites (no rename; slot declared at migration time):

| Site | Slot | Rationale |
|---|---|---|
| routes.rs:3286 (semantic keyword rewrite) | `mid` | Fast focused rewrite |
| routes_ask.rs:498 (question synthesis) | `max` | Judgment work for user-facing answer |
| routes.rs:8352 (navigate synthesis) | `max` | Judgment work |
| migration_config.rs:587 | `mid` | Config generation |
| generative_config.rs:253 | `mid` | Config generation |
| delta.rs × 4 | TBD Wave 4 (read per-site intent) | Likely `mid` |
| faq.rs × 6 | TBD Wave 4 | Likely `mid` |
| meta.rs × 4 | TBD Wave 4 | Likely `mid` |
| webbing.rs × 1 | TBD Wave 4 | Likely `mid` |

---

## 3. What changes vs what survives

### Added

- **Canonical StepContext constructor** `make_step_ctx_from_llm_config` (rename from `_for_slot_and_model`). Required slot parameter. Always attaches Decision via `with_dispatch_decision_if_available`.
- **Runtime guard** at `call_model_unified_with_audit_and_ctx` entry: if `ctx.dispatch_decision.is_none() && options.model_override.is_none()` → `tracing::error!(event="walker_dispatch_spine_missing", step_name, primitive)` + return `Err(anyhow!("dispatch missing both Decision and model_override; call site must use make_step_ctx_from_llm_config with a slot"))`.
- **Bundled seed entries** for 4 new tiers in `walker_provider_openrouter` (§2 Decision 1).
- **Regression fence test** `walker_dispatch_spine_enforced` in `src-tauri/tests/walker_spine_enforcement.rs`. Greps source files for `StepContext::new(` outside `#[cfg(test)]` modules and for the deleted helper names; asserts zero hits.

### Deleted

- `make_step_ctx_from_llm_config` (non-slot variant at `step_context.rs:572`). Merged with the canonical name.
- `make_step_ctx_from_llm_config_with_model` (W3c workaround at `step_context.rs:598`). Replaced by canonical.
- `walker_resolver::first_openrouter_model_from_db` — past-me's pragmatic bypass. Delete after migration proves no callers remain.
- All `LlmCallOptions::default()` call sites that pair with manual `StepContext::new` Category B pattern — replaced by canonical helper; `model_override` coexists with Decision where slug-pinning is needed.

### Preserved

- `chain_dispatch::dispatch_llm` / `dispatch_ir_llm` Decision-attach-at-outer-step-entry (Phase 1 W1b). Untouched.
- Clone-propagation of `Arc<DispatchDecision>` downstream in chain context. Untouched.
- `LlmCallOptions.model_override` as per-call slug pin. Continues to win over Decision's `model_list[0]` WITHIN the cascade's chosen provider. Not a bypass; compatible with Decision.
- `LlmConfig::prepare_for_replay` for inbound-job replay paths. Already clears dispatch-routing fields; compatible with canonical.
- `walker_resolver` typed accessors + readiness stubs. Untouched.

---

## 4. Canonical API design

### 4.1 The sole constructor

```rust
pub async fn make_step_ctx_from_llm_config(
    config: &LlmConfig,
    step_name: &str,
    primitive: &str,
    depth: i64,
    chunk_index: Option<i64>,
    system_prompt: &str,
    slot: &str,                  // REQUIRED — walker tier name
    model: Option<&str>,         // optional pre-resolved slug
    provider_id: Option<&str>,   // optional pre-resolved provider
) -> Option<StepContext>
```

Behavior (unchanged from current `_for_slot_and_model`):
- Constructs StepContext with `model_tier = slot`, `resolved_model_id = model`, `resolved_provider_id = provider_id`.
- Calls `with_dispatch_decision_if_available(ctx).await` which builds `DispatchDecision` for the slot via `walker_resolver::DispatchDecision::build_with_build_id`.
- Returns `Some(ctx)` with Decision attached if build succeeds; returns `Some(ctx)` without Decision if registry has no such tier (permissive-on-failure — runtime guard catches this at walker entry).

### 4.2 Call-site pattern (canonical)

```rust
// Resolve tier through provider registry first (gives both model_id + provider_id).
let resolved = llm_config
    .provider_registry
    .as_ref()
    .and_then(|reg| reg.resolve_tier(slot, None, None, None).ok())
    .ok_or_else(|| anyhow!("{caller}: provider registry has no '{slot}' tier"))?;

let cache_ctx = make_step_ctx_from_llm_config(
    llm_config,
    step_name,
    primitive,
    depth,
    chunk_index,
    system_prompt,
    slot,
    Some(&resolved.tier.model_id),
    Some(&resolved.provider.id),
).await;

let response = llm::call_model_unified_and_ctx(
    llm_config,
    cache_ctx.as_ref(),
    system_prompt,
    user_prompt,
    temp,
    max_tokens,
    None,
).await?;
```

### 4.3 model_override coexistence

For sites that need to pin a specific slug (replay paths, fleet-received workers, market-received workers), the pattern is:

```rust
let cache_ctx = make_step_ctx_from_llm_config(.., slot, Some(&pinned_slug), ..).await;
let response = call_model_unified_with_options_and_ctx(
    llm_config, cache_ctx.as_ref(), .., LlmCallOptions {
        model_override: Some(pinned_slug.to_string()),
        ..Default::default()
    }
).await?;
```

Decision is attached (cascade works), AND `model_override` pins the slug within whichever provider the cascade selects.

---

## 5. Runtime guard design

Insert at the head of `call_model_unified_with_audit_and_ctx` (llm.rs), AFTER cache probe but BEFORE provider walker loop:

```rust
// Walker-v3-completion guard: dispatch spine missing means silent
// bypass. Fail loud instead of iterating providers and skipping each
// with walker_v3_no_model.
if ctx.and_then(|c| c.dispatch_decision.as_ref()).is_none()
    && options.model_override.is_none()
{
    let step_name = ctx.map(|c| c.step_name.as_str()).unwrap_or("<no_ctx>");
    let primitive = ctx.map(|c| c.primitive.as_str()).unwrap_or("<no_ctx>");
    tracing::error!(
        event = "walker_dispatch_spine_missing",
        step_name = %step_name,
        primitive = %primitive,
        "walker-v3-completion: call site has neither DispatchDecision \
         nor LlmCallOptions.model_override. Use make_step_ctx_from_llm_config \
         with a slot.",
    );
    return Err(anyhow!(
        "walker dispatch spine missing: step_name={step_name}, \
         primitive={primitive}. Call site must use \
         make_step_ctx_from_llm_config with a slot (Walker v3 Completion)."
    ));
}
```

Positioned AFTER cache probe because cache hits must still work even if the ctx is malformed (cache probe is purely data-retrieval; dispatch spine matters only for miss-path dispatch).

---

## 6. Waves

Wave-by-wave commits. Each wave ends with `cargo check` (default target), `cargo test --lib` (no new regressions; 15 pre-existing failures unchanged), and a named commit referencing the wave.

### Wave 0 — prereqs + verify three unknowns

1. **Verify my 4-site fix unblocks Adam's build end-to-end.** Let a build run; watch chronicle for `walker_resolved` events at extraction_schema / synthesis_prompts / supersession / vine steps. Confirm no `walker_v3_no_model` events for those specific steps.

2. **Trace evidence_answering's pre_map call path to verify Decision is None.** Add `tracing::debug!` at evidence_answering.rs:329 logging `ctx.dispatch_decision.is_some()`. Run a build. Confirm the debug log shows `false`. Remove the debug log.

3. **Verify walker behavior when Decision=None + model_override=Some.** Add `tracing::debug!` at llm.rs walker entry logging both fields. Run a build from an existing stale-check path (e.g., trigger stale_helpers via touching a file in a synced folder). Confirm walker falls through OpenRouter via model_override while skipping Market/Fleet with `walker_v3_no_model`.

4. **Test baseline snapshot.** Run `cargo test --lib` → record pass/fail counts for regression comparison.

5. **Bundled seed preview.** Draft the updated `walker_provider_openrouter` bundled_contributions.json entry with Adam's 4 new tiers + explicit context_limits. Validate JSON shape. Do NOT commit yet — lands in Wave 1.

**Exit criterion:** three unknowns resolved (verified or negated). Test baseline recorded. Bundled-seed draft validated.

### Wave 1 — canonicalize the constructor + tier seed

Single commit: `refactor(walker-v3-completion): canonical StepContext constructor + 4 new tiers`

1. Rename `make_step_ctx_from_llm_config_for_slot_and_model` → `make_step_ctx_from_llm_config` in `step_context.rs`.
2. Delete the existing non-slot `make_step_ctx_from_llm_config` (step_context.rs:572).
3. Delete `make_step_ctx_from_llm_config_with_model` (step_context.rs:598).
4. Update callers from my 4-site fix (extraction_schema.rs × 2, supersession.rs × 1, build.rs × 1) to the canonical name.
5. Add 4 new tier entries to `walker_provider_openrouter` bundled seed in `src-tauri/assets/bundled_contributions.json`:
   ```yaml
   model_list:
     evidence_loop: ["xiaomi/mimo-v2.5-pro"]
     stale_l0: ["minimax/minimax-m2.7"]
     stale_upper: ["xiaomi/mimo-v2.5-pro"]
     reroll: ["xiaomi/mimo-v2.5-pro"]
   context_limit:
     evidence_loop: 1000000
     stale_l0: 197000
     stale_upper: 1000000
     reroll: 1000000
   ```
6. Update `test_bundled_tier_coverage_strict` (or equivalent) to assert all 10 tiers present.
7. `cargo check` + `cargo test --lib`. Commit.

**Exit criterion:** Compilation clean, tests green. No site uses deleted helpers. Four new tiers resolve via `provider_registry.resolve_tier(slot, ...)`.

### Wave 2 — migrate Category A (ctx=None sites, 3 sites)

Single commit: `feat(walker-v3-completion): migrate Category A ctx=None sites to canonical helper`

1. `routes.rs:3286` (semantic keyword rewrite) → canonical helper, slot=`mid`.
2. `routes_ask.rs:498` (question synthesis) → canonical helper, slot=`max`.
3. `routes.rs:8352` (navigate synthesis) → canonical helper, slot=`max`.
4. `cargo check` + `cargo test --lib`. Commit.

### Wave 3 — migrate Category B (manual StepContext::new, ~19 sites)

Per-file commits (each file one commit) for easier review and bisect:

1. `evidence_answering.rs` × 4 → slot=`evidence_loop` (with Rust-side slot literal renamed). Commit.
2. `stale_helpers.rs` × 4 → slot=`stale_l0`. Commit.
3. `stale_helpers_upper.rs` × 6 → slot=`stale_upper`. Commit.
4. `migration_config.rs:587` → slot=`mid`. Commit.
5. `generative_config.rs:253` → slot=`mid`. Commit.
6. `reroll.rs:155` → slot=`reroll`. Commit.

After each file commit: `cargo check` + targeted `cargo test --lib <module>`.

### Wave 4 — migrate Category C (_with_model workaround, ~15 sites)

Per-file commits. For each site, read the surrounding code to determine declared slot intent; default to `mid` if unclear.

1. `delta.rs` × 4 — read each site, pick slot. Commit.
2. `faq.rs` × 6 — read each site. Commit.
3. `meta.rs` × 4 — read each site. Commit.
4. `webbing.rs` × 1. Commit.

If any site's declared intent is ambiguous (e.g., delta.rs's `delta_thread_match` could reasonably be `mid` or `extractor`), pause and surface to Adam before committing. Heuristic: `mid` (mercury-2) for default work, `extractor` for L0-shape extraction, `max` for judgment work.

### Wave 5 — runtime guard at walker entry

Single commit: `feat(walker-v3-completion): runtime guard — fail loud on missing dispatch spine`

1. Insert guard per §5 at `call_model_unified_with_audit_and_ctx`.
2. Add `#[test] walker_entry_fails_loud_on_no_decision_no_override` asserting the error fires.
3. Audit existing tests for any that intentionally relied on tolerant pass-through — update each to construct a proper Decision or set `model_override`.
4. `cargo check` + `cargo test --lib`. Commit.

### Wave 6 — regression fence test + delete pragmatic bypass

Single commit: `test(walker-v3-completion): spine-enforcement regression fence + delete first_openrouter_model_from_db`

1. New `src-tauri/tests/walker_spine_enforcement.rs`:
   - Greps `src/` for non-test `StepContext::new(` — asserts zero occurrences OR each is followed by `with_dispatch_decision_if_available` within 20 lines.
   - Greps for deleted helper names `make_step_ctx_from_llm_config_with_model` — asserts zero.
2. Delete `walker_resolver::first_openrouter_model_from_db` if no callers remain post-migration.
3. `cargo check` + `cargo test`. Commit.

### Wave 7 — verification gate

1. Wanderer re-audit, same prompt as before: *"Does every LLM dispatch path in `pyramid/` route through walker Decision?"* Target verdict: clean.
2. Dev smoke on Adam's machine: run a real build end-to-end. Chronicle query: walker_resolved events present at every chain step; zero `walker_v3_no_model` events across evidence_loop / stale_upper / stale_l0 / reroll / faq / delta / meta / webbing. Chronicle supersession/vine steps route correctly.
3. If anything fails, hotfix commit + re-verify.

---

## 7. Acceptance criteria

- Zero non-test call sites use deleted helpers (`make_step_ctx_from_llm_config` non-slot, `make_step_ctx_from_llm_config_with_model`, `first_openrouter_model_from_db`).
- Zero non-test `StepContext::new` constructions for LLM dispatch (verified by regression fence test).
- Every call site that reaches `call_model_unified*` has either (a) Decision attached via canonical helper, or (b) `LlmCallOptions.model_override` set explicitly. Both is also valid.
- Runtime guard at walker entry fails loud on (no Decision + no model_override); test proves.
- `cargo check` default target clean.
- `cargo test --lib` regression baseline unchanged (15 pre-existing fails still 15; no new fails).
- Regression fence test passes.
- Wanderer re-audit returns clean verdict.
- Adam's failing build runs end-to-end with zero `walker_v3_no_model` events across all migrated sites.

---

## 8. What is explicitly NOT in scope

- **Inference-routing-v2 §0 walker_policy contribution family** (walker_phase_policy, walker_body_templates, walker_error_classification, walker_retry_policy as first-class contributions making Rust a dumb executor). Multi-session architectural work; different plan.
- **YAML chain migration of evidence_loop / extraction_schema / supersession / stale_engine** (converting Rust-hardcoded flow to declarative chain steps). Different session.
- **Promoting any Rust-side tier names beyond the 4 committed in §2** (Decision 1). If future semantic distinctions surface, supersede the bundled seed.
- **Category C declared-intent decisions beyond what Wave 4 surfaces to Adam.** Wave 4 may pause per-site if intent is ambiguous.
- **Refactoring walker branch-selection logic** (llm.rs:2402 / 3219 / 3647 Market/Fleet/OR branches). Already canonical under W1b.
- **Operator HTTP routes / CLI surfaces** (`routes_operator.rs`, etc.) — not verified to be on the walker dispatch path. If Wave 0 trace surfaces bypass there, add to Wave 2 scope via ledger.

---

## 9. LOC + session estimate

- Touched files: ~14 (step_context.rs + llm.rs + bundled_contributions.json + ~11 migration targets)
- Constructor consolidation: ~60 LOC net (delete 2 helpers, rename 1, update 7 callers)
- Site migrations: ~20 LOC × ~30 sites ≈ ~600 LOC
- Runtime guard + test: ~40 LOC
- Regression fence test: ~100 LOC
- Bundled seed update: ~20 LOC JSON
- **Total: ~800 LOC net diff, 1 session realistic**
- ~12 commits (1 Wave 0, 1 Wave 1, 1 Wave 2, 6 Wave 3, 4 Wave 4, 1 Wave 5, 1 Wave 6)

---

## 10. Open unknowns for Wave 0 verification

These are the three things past-me flagged as untraced assumptions:

1. **Does my 4-site fix actually address the critical path?** Past-me doesn't recognize "4-site fix" as a phase. Verification: run a build, confirm walker_resolved at synthesis_prompts / extraction_schema / supersession / vine steps.

2. **Does evidence_answering's pre_map reach the walker with Decision=None?** Verification: add trace log at line 329, observe during a build, confirm log shows `dispatch_decision.is_some() == false`.

3. **Walker behavior when Decision=None + model_override=Some?** Verification: trigger a stale-check path, confirm walker falls through OpenRouter via override while Market/Fleet skip silently.

If Wave 0 surfaces that (1) is false (my 4-site fix didn't unblock the path), Wave 1 scope expands to include the residual bypass. If (2) is false (evidence_answering does inherit Decision via some inheritance path I missed), that reduces Category B scope by 4 sites. If (3) is false (walker fails entirely when Decision=None, regardless of model_override), that changes the runtime guard's error message wording but not its location.

---

## 11. Commit message convention

All commits: `<type>(walker-v3-completion): <Wave N — short summary>`. Examples:
- `refactor(walker-v3-completion): Wave 1 — canonical StepContext constructor + 4 new tiers`
- `feat(walker-v3-completion): Wave 3 — evidence_answering migrated to slot=evidence_loop`
- `test(walker-v3-completion): Wave 6 — spine-enforcement regression fence`

Commit messages reference this plan + relevant §. Same pattern as walker-v3-shipping branch convention.
