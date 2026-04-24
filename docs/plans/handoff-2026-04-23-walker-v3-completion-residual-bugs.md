# Handoff — Walker V3 Completion: Residual Bugs After Ship

**Date**: 2026-04-23 (end of session)
**Branch**: `walker-v3-shipping` (not merged to main)
**Last commit**: `399a8a1` — Wave 7 hotfix (delete call_model_direct + migrate 2 residual bypass sites)
**Status**: Walker-v3-completion structural work SHIPPED and works. Three residual bugs identified during dev smoke.

---

## 1. What shipped (don't re-do this work)

Plan: `docs/plans/walker-v3-completion-decision-attachment.md` rev 1.0. Seven waves, 10 commits:

| Wave | Commit | What |
|---|---|---|
| 1 | `3ce748f` | Canonical `make_step_ctx_from_llm_config(.., slot, model?, provider_id?)` constructor; deleted non-slot variant; added 4 first-class tiers to bundled `walker_provider_openrouter` (evidence_loop, stale_l0, stale_upper, reroll) with explicit context limits |
| 2 | `23b3970` | 3 Category-A ctx=None sites migrated + 5-item dead chain deleted in `routes_ask.rs` |
| 3a–3d | `725891f` `dc3a1be` `2b82fb5` `c41f902` | 19 Category-B manual StepContext::new sites migrated (evidence_answering, stale_helpers, stale_helpers_upper, migration_config, generative_config, reroll) |
| 4a–4d | `930f791` `f4da3ad` `df4f854` `9d6e1cd` | 15 Category-C `_with_model` sites migrated (webbing, delta, faq, meta) |
| 5 | `5d8de35` | Runtime guard at `call_model_unified_with_audit_and_ctx` entry: fails loud when `ctx.dispatch_decision.is_none() && options.model_override.is_none()`; 2 guard tests |
| 6 | `a6e823d` | Deleted `make_step_ctx_from_llm_config_with_model` helper; added `tests/walker_spine_enforcement.rs` regression fence |
| 7 | `399a8a1` | Post-wanderer hotfix: deleted `call_model_direct` (192 LOC — silent bypass hole); migrated ascii_art banner + planner_call |

**Gates at ship**: `cargo check --lib` clean, `cargo test --lib` 1967 pass / 15 pre-existing fail (baseline held across all 10 commits), `cargo test --test walker_spine_enforcement` 2/2 pass.

**Proven good by dev smoke on slug `architecturewalkerv3test8`**:
- Canonical dispatch spine works — every step gets a Decision
- Runtime guard fires correctly (identified Bug 1 below at the exact failure point, with actionable error message pointing at the canonical helper)
- Walker cascade advances correctly on market deadline expiry
- `walker_exhausted` emits loudly
- Rust-level retry loop in `extraction_schema.rs::generate_synthesis_prompts` rescued a 10-minute walker hang when first attempt exhausted

## 2. Three residual bugs

### Bug 1 — Runtime: operator's existing `walker_provider_openrouter` supersession doesn't have the new tier names

**Symptom**: dev smoke fired `walker_dispatch_spine_missing` on `pre_map_layer` (evidence_answering) with `step_name=<no_ctx>`.

**Root cause**: at runtime, Wave 3a's migrated `evidence_answering.rs:303` calls `make_step_ctx_from_llm_config(.., "evidence_loop", ..)` which internally does `provider_registry.resolve_tier("evidence_loop", None, None, None)`. On Adam's install, there's an active user-authored `walker_provider_openrouter` supersession (contribution id `752b78cf-9e06-4e5e-a294-b0bf4f519edd`, 710 bytes) that has tier names from an older naming scheme: `extractor`, `fast_extract`, `high`, `max`, `mid`, `stale_local`, `stale_remote`, `synth_heavy`, `web`. It does NOT have: `evidence_loop`, `stale_l0`, `stale_upper`, `reroll`.

At scope 4 (provider), user's supersession wins over the bundled seed. `resolve_tier` returns None for the new tiers → `make_step_ctx_from_llm_config` returns None (because pre_map_resolved is None) → ctx reaches walker as None → options.model_override is None (`LlmCallOptions::default()`) → Wave 5 guard fires.

**Verify on any install**:

```bash
DB=~/Library/Application\ Support/wire-node/pyramid.db
sqlite3 "$DB" "SELECT contribution_id, length(yaml_content) FROM pyramid_config_contributions
  WHERE schema_type='walker_provider_openrouter' AND status='active' AND superseded_by_id IS NULL"
# If more than one row OR a user-authored row (not source='bundled'), check its model_list keys:
sqlite3 "$DB" "SELECT yaml_content FROM pyramid_config_contributions
  WHERE schema_type='walker_provider_openrouter' AND status='active' AND superseded_by_id IS NULL
    AND source != 'bundled' LIMIT 1"
```

If the tier keys don't include `evidence_loop`, `stale_l0`, `stale_upper`, `reroll` → Bug 1 manifests.

**Fix options** (pick one):

1. **Operator regenerates supersession** to include the new tier names. Copy the existing yaml, add:
   ```yaml
     model_list:
       # ... existing keys ...
       evidence_loop: ["xiaomi/mimo-v2.5-pro"]
       stale_l0: ["minimax/minimax-m2.7"]
       stale_upper: ["xiaomi/mimo-v2.5-pro"]
       reroll: ["xiaomi/mimo-v2.5-pro"]
     context_limit:
       # ... existing keys ...
       evidence_loop: 1000000
       stale_l0: 197000
       stale_upper: 1000000
       reroll: 1000000
   ```
   Then write via the supersession IPC handler.

2. **Operator deletes current supersession** so the bundled seed (which has all 10 tiers since Wave 1) wins at scope 4. Simplest — 1 DB row update to set `status='retracted'`.

3. **Code fix (architecturally right)**: make the walker resolver's `resolve_model_list` cascade to scope-5 bundled when scope-4 user supersession exists but doesn't define the requested slot. Currently scope 4 is winner-takes-all per `walker_resolver.rs::resolve_model_list`. Harder; out of scope for a quick unblock but the correct long-term move.

**Files to read for Bug 1**:
- `src-tauri/src/pyramid/walker_resolver.rs` — `resolve_model_list` function, look at scope chain
- `src-tauri/src/pyramid/evidence_answering.rs:303,1037,1540,1754` — migrated sites
- `src-tauri/src/pyramid/step_context.rs` — `make_step_ctx_from_llm_config` + `with_dispatch_decision_if_available`
- `src-tauri/src/pyramid/llm.rs:2183` — guard position (AFTER cache probe, BEFORE walker loop)

### Bug 2 — Chain executor silently swallows evidence_loop per-layer failures

**Symptom**: build reports `status='completed', 17/17 steps` but only L0 nodes exist; `layers_completed=0, total_layers=0, total_node_count=0` in `pyramid_builds`.

**Root cause**: when `pre_map_layer` or `answer_questions` in `execute_evidence_loop` returns Err (which it did on Bug 1's guard fire), the chain executor catches the Err, emits `tracing::warn!`, sets `build_error = Some(msg)`, and `break`s out of the layer loop. The function then returns `Ok(...)` to chain_executor, and subsequent steps (gap_processing, l1_webbing, l2_webbing) run against the empty pyramid producing "0 nodes, 0 edges" results. The outer chain reports `completed, failures=0`.

**Files + line ranges**:
- `src-tauri/src/pyramid/chain_executor.rs:5870-5889` — `pre_map_layer` error handling: `match super::evidence_answering::pre_map_layer(...).await { Ok(m) => m, Err(e) => { warn!(...); build_error = Some(...); break; } }`
- `src-tauri/src/pyramid/chain_executor.rs:5907-5932` — same pattern for `answer_questions`

**Fix**:

Translate `build_error.is_some()` post-loop into an `Err(...)` return from `execute_evidence_loop`. Mechanical edit, small diff. Consider whether to attribute the failed_layer index in the error message. Verify: after fix, the build should mark `pyramid_builds.status='failed'` when pre_map_layer dies on a configured-but-missing tier, instead of silently "completing" with L0-only.

**Why my Wave 6 regression fence didn't catch this**: the fence asserts *structural* bypass (`StepContext::new` without Decision attach, deleted helper references). Error-propagation bugs in chain_executor aren't structural — they're control-flow semantics. Fence doesn't cover them. A companion assertion — "every `warn!` + `break` pattern in chain_executor.rs must be paired with either a `return Err` or a post-loop check" — would be a reasonable Wave 6b.

### Bug 3 — Market delivery callback URL caches forever (pre-existing, unrelated to walker-v3-completion)

**Symptom**: on downstairs (provider), repeated `market_result_delivery_failed` + `market_settlement_delivery_attempt_failed` events against the same job_ids. Upstairs shows 0 `network_result_returned` despite 17 market purchases. Only 1 delivery succeeded — `c1f1767d` at 05:02:58, which was BEFORE upstairs's binary swap.

**Root cause**: at `/fill` time, upstairs captures `callback_url = format!("{}/v1/compute/job-result", tunnel.tunnel_url)` (`llm.rs:2394-2406`) and sends it in the /fill body. Downstairs persists this to `fleet_result_outbox.requester_callback_url` (`db.rs:2879-2908`). `market_delivery.rs:802-819 deliver_leg` POSTs to that cached URL forever. When upstairs swapped binary, tunnel URL changed → all in-flight outbox rows on downstairs hold the stale URL → POST fails with transport error (connection refused / DNS).

Grep for `tunnel_url_changed`, `refresh.*callback`, `update.*callback_url`, `re.?resolve.*url` returns zero matches across the tree. No recovery logic exists.

**Fix options** (pick one):

1. Re-resolve callback URL at delivery time from an upstairs-identity → current-tunnel lookup.
2. Invalidation: when upstairs's tunnel URL rotates, have it broadcast a "forget pending callbacks, I'm here now" beacon that downstairs uses to update outbox rows.
3. Use a stable URL (operator-handle-scoped) instead of tunnel-URL-scoped.

Scope: SEPARATE from walker-v3-completion. Own plan doc. Own session.

## 3. What's NOT broken (don't waste time re-verifying)

- Walker v3 canonical spine — proven by 8 LLM audit rows routing through canonical helper, guard never fired spuriously
- Dispatch Decision attachment — proven by cascade advancing correctly on market-deadline expiry
- Cascade order (Market → Fleet → OpenRouter → Local) — proven by chronicle's walker_resolved events picking OpenRouter after market fails
- Runtime guard — proven by correctly firing on Bug 1 with actionable message (caller pointed at `make_step_ctx_from_llm_config` + slot)
- Walker exhaust — proven by `walker_exhausted` event firing after cascade truly runs out
- Retry loop in `extraction_schema.rs` — proven by rescuing a 10-min hang with second-attempt OpenRouter success
- Regression fence — proven by catching `CacheStepContext::new` false positive during construction, ultimately passing with word-boundary match
- `_with_model` helper deletion — proven by `cargo test --test walker_spine_enforcement` + zero callers

## 4. Reproduction

**Install**: walker-v3-shipping binary at `/Users/adamlevine/AI Project Files/agent-wire-node-walker/src-tauri/target/release/bundle/macos/Agent Wire Node.app`, running against DB at `~/Library/Application Support/wire-node/pyramid.db`.

**Trigger Bug 1 + Bug 2**: start any build that reaches evidence_loop (L1+ synthesis) — e.g., a Question Pipeline chain against a multi-document corpus. Bug 1 fires on `pre_map_layer`'s first LLM call; Bug 2 masks it as a successful build.

Dev smoke build slug: `architecturewalkerv3test8` (started 2026-04-24 00:00, completed 00:21, 17/17 steps, 5 L0 nodes, no L1+).

**DB queries for quick diagnosis**:

```bash
DB=~/Library/Application\ Support/wire-node/pyramid.db

# Bug 1 symptom — dispatch-spine errors in tracing log / logs from walker_dispatch_spine_missing events
# (No direct DB row; only in tracing::error output and returned Err messages)

# Bug 2 symptom — builds that report completed but have fewer nodes than chain steps
sqlite3 "$DB" "SELECT slug, status, layers_completed, total_node_count FROM pyramid_builds
  WHERE status='completed' AND total_node_count <= 10 ORDER BY started_at DESC LIMIT 5"

# Actual node count by layer
sqlite3 "$DB" "SELECT depth, COUNT(*) FROM pyramid_nodes WHERE slug='<slug>' GROUP BY depth"

# LLM audit rows (only calls that actually dispatched)
sqlite3 "$DB" "SELECT step_name, COUNT(*), SUM(CASE WHEN status='complete' THEN 1 ELSE 0 END)
  FROM pyramid_llm_audit WHERE slug='<slug>' GROUP BY step_name"

# Chronicle event breakdown for walker behavior
sqlite3 "$DB" "SELECT event_type, COUNT(*) FROM pyramid_compute_events WHERE slug='<slug>' GROUP BY event_type"
```

## 5. Suggested bug-fix order

1. **Bug 1 (a) or (b)** — unblock the test environment. 5 min.
2. **Bug 2** — small chain_executor edit. Make the failure the build actually failed. ~30 min including test.
3. **Re-run smoke build** — should now either (a) build end-to-end with full L1+/L2+ synthesis, or (b) fail loud with a real error message tied to which layer + which step.
4. **Bug 1 (c)** — scope-5 cascade for model_list. ~2 hours including tests. Systemic fix but not blocking.
5. **Bug 3** — separate plan, separate session. Not walker-v3-completion scope.

## 6. Out of scope for this handoff

- The 11 `walker_resolver::first_openrouter_model_from_db` bootstrap-resolver callers across main.rs / server.rs / routes.rs / chain_executor.rs. These are pre-flight tier-resolution helpers used to pass a model as `LlmCallOptions.model_override` downstream. NOT walker bypasses (the downstream dispatches are canonical post-Wave-3/4). Plan §10 noted these for future replacement with `DispatchDecision::synthetic_for_preview`; leaving as-is.
- Merge of main-branch synthesis work onto walker-v3-shipping. That's a separate gate: run `cargo test --test walker_spine_enforcement` post-merge; fence will catch any new bypass sites the synthesis work introduced.
- Environmental `../dist` issue (frontend build output symlink missing): walker-v3-completion plan §6 Wave 6 noted this requires `npm run build` in the frontend dir before `cargo test` default-target works. Integration tests can be run with `cargo test --lib` as a workaround (doesn't need the Tauri binary).

## 7. Contact / context

- Walker-v3-completion plan: `docs/plans/walker-v3-completion-decision-attachment.md` rev 1.0
- Walker v3 design plan (upstream): `docs/plans/walker-provider-configs-and-slot-policy-v3.md` rev 1.0.2
- Walker re-plan (immediate predecessor): `docs/plans/walker-re-plan-wire-2.1.md` rev 0.3
- Walker impl log: `docs/plans/walker-re-plan-wire-2.1-IMPL-LOG.md`
- Walker friction log: `docs/plans/walker-re-plan-wire-2.1-FRICTION-LOG.md`
- Regression fence test: `src-tauri/tests/walker_spine_enforcement.rs`
- Bundled contributions (4 new tier seeds): `src-tauri/assets/bundled_contributions.json` → `bundled-walker_provider_openrouter-default-v1`

## 8. Handoff checklist

- [ ] Operator confirms which Bug 1 fix path (a/b/c)
- [ ] Bug 2 fix committed as Wave 6b
- [ ] Re-run dev smoke against test corpus after Bug 1 + Bug 2 fixes
- [ ] Confirm pyramid builds past L0 — audit rows for synthesis_prompts, evidence_loop (pre_map + answer + merge + target), stale_helpers, delta/faq/meta/webbing (only if those chain steps run in the test)
- [ ] `cargo test --test walker_spine_enforcement` still passes
- [ ] `cargo test --lib` still 1967/15 baseline
- [ ] Bug 3 scoped into its own plan doc for future session
