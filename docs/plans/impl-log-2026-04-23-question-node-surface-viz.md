# Implementation Log: Question Node Surface Viz

**Date:** 2026-04-23  
**Companion:** `docs/plans/handoff-2026-04-23-question-node-surface-viz.md`

Keep this file short and operational. 

## Goal

Make question nodes visible and inspectable as first-class Pyramid Surface objects:

```text
question appears -> answer attaches -> evidence/gaps/provenance attach
```

without hardcoding `decompose` in React. YAML declares structural visualization; backend exposes question nodes as typed read-model objects; frontend renders/inspects the generic object.

## Progress

- Created this implementation log.
- 2026-04-24 tester regression fix: replaced opaque `q-{hash}` question IDs with user-facing layer handles (`Q-L{layer}-{index}`) in question decomposition. The allocator still de-dupes repeated semantic questions before issuing a new handle, preserving the DAG property that one child can attach to multiple parents.
- Updated `chains/defaults/question.yaml`: `decompose` and `decompose_delta` now declare `viz.type: node_fill` with `source: question_nodes` and `node_kind: question` instead of `progress_only`.
- Began backend read-model patch:
  - Extended `TreeNode` with optional `node_kind`, question metadata, answer linkage fields, and answered state.
  - Extended `LiveNodeInfo` with optional `node_kind`, question metadata, answer linkage fields, and answered state.
  - Extended `DrillResult` with optional `question_node` and `linked_answer`.
  - Added `QuestionNodeDetail` to `types.rs`.
  - Updated existing `TreeNode` and `LiveNodeInfo` Rust literals to populate new optional fields with `None`.
  - Extended `QuestionNodeRow` with optional `build_id` and `created_at`.
  - Added DB helpers in `db.rs`:
    - `question_row_child_ids`
    - `question_visual_depth`
    - `get_question_node`
    - `get_answer_node_for_question`
- Spawned high-powered subagents to avoid context churn:
  - Backend worker owns `src-tauri/src/pyramid/db.rs`, `query.rs`, and `types.rs`.
  - Frontend worker owns Pyramid Surface and Theatre inspector TypeScript paths.
  - QA explorer is read-only and will report integration risks.
- Completed backend read-model patch:
  - `pyramid_build_live_nodes` now includes question-node projections from `pyramid_question_nodes`.
  - `pyramid_tree` now includes typed question `TreeNode` projections.
  - `pyramid_drill` now resolves question rows before answer nodes and returns a synthetic drill result with `question_node` and optional `linked_answer`.
  - Question visual depth is normalized from stored root-down depth to display depth, keeping L0 reserved for extracted/source nodes.
- Completed frontend surface/inspector patch:
  - Question metadata and linked-answer fields are preserved through tree flattening, live-node mapping, layout, and `SurfaceNode`.
  - Live question nodes are merged into the surface even when a previous answer tree payload exists.
  - Upper answer-node duplicates are suppressed when the matching question node already owns the linked answer.
  - Tooltips and inspector details are question-first, then answer/evidence/gaps/provenance.
  - Removed a temporary nonexistent IPC fallback; `pyramid_drill` is now the question-node drill path.
- Completed incremental question-node visibility patch:
  - Question IDs are now derived from question text, scope, and root-down tree depth rather than final visual layer depth, so finalized subtrees can keep stable IDs before the whole decomposition tree is known.
  - Fresh incremental decomposition now persists each finalized question subtree to `pyramid_question_nodes` during recursive decomposition and emits `NodeProduced` events for those question IDs.
  - The final reviewed apex tree is saved in one transaction with legacy question rows cleared first, so candidate rows invalidated by apex-level sibling review are removed before the build moves on.
- Earlier validation:
  - `cargo fmt`
  - `cargo test --bin wire-node-desktop --no-run` passed with existing warning noise.
  - `npm run build` passed with the existing large chunk warning.
- Incremental question pass validation:
  - `cargo fmt`
  - `cargo test --bin wire-node-desktop --no-run` passed with existing warning noise.

## Remaining Structural Follow-Ups

- Durable answer linkage still needs a real `question_id` / `source_question_id` on answer materialization. Current read model bridges by `(slug, visual_depth, self_prompt == question)`.
- Question-node persistence is no longer purely batch-oriented for fresh decompositions: finalized subtrees are persisted and emitted during recursion. Caveat: sibling review can still briefly expose candidate rows during decomposition; the final reviewed apex save clears stale candidate rows in the same transaction.
- Build scoping for `pyramid_question_nodes` remains legacy slug-scoped. `build_id` is now carried in read-model output, but the table primary key is still `(slug, question_id)`.
- YAML now declares `viz.source: question_nodes` / `node_kind: question`; the read model exposes those objects generically. A future refinement can make the frontend consume the full viz metadata object, not just `viz.type`.

## Follow-Up Pass In Progress

- Spawned focused workers:
  - Durable Linkage: add stable `source_question_id` on answer materialization and prefer it in read models.
  - Incremental Questions: persist/emit question nodes earlier during recursive decomposition where the existing id flow allows it.
  - Viz Metadata: expose the full YAML `step.viz` metadata through the frontend mapping/render path, not just `viz.type`.
- Integrated follow-up results:
  - New answer materialization stamps `PyramidNode.source_question_id` from `LayerQuestion.question_id`.
  - Read models prefer `source_question_id` and keep the old text/depth bridge as a legacy fallback.
  - Incremental decomposition assigns question IDs from root-down tree depth, persists finalized subtrees as they complete, and emits `NodeProduced` for newly-created question rows.
  - YAML viz metadata is preserved as a full `VizStepConfig` and passed through the data/render path.
  - When folding answer nodes into question nodes, the surface rewrites DAG edges through the owning question id and deduplicates exact duplicate edges only, instead of dropping evidence/structural connections.
- Follow-up validation passed:
  - `cargo fmt`
  - `cargo test --bin wire-node-desktop --no-run` passed with existing warning noise.
  - `npm run build` passed with the existing large chunk warning.
  - `git diff --check` passed.

## Mid-Patch State / Resume Notes

The mid-patch state and structural follow-up pass are resolved. On resume, re-read this log and verify whether the running desktop app has picked up the updated chain YAML; source changes alone may not affect an already-installed app until runtime bundled/default chain files are refreshed or the app is rebuilt/restarted.

## Runtime Test 2026-04-23: Market Worked, Source Extract Failed On Cached Malformed JSON

- User test slug: `architecturewalkerv3test11`.
- Result: market quote/purchase/result-return worked for `gemma4:26b`, but build failed during `source_extract` before completing the L0 row.
- Root cause from runtime DB:
  - `pyramid_builds.error_message`: `forEach abort at index 3: Step 'source_extract': JSON parse failed after self-healing`.
  - `source_extract` item 3 returned malformed fenced JSON, then `source_extract_heal_standard` also returned malformed JSON.
  - The raw malformed LLM responses were stored in `pyramid_step_cache` before `chain_dispatch` parsed step JSON.
  - `on_error: retry(3)` existed, but retries hit the same bad cache rows, so retry became deterministic replay instead of a fresh market/local attempt.
  - `pyramid_llm_audit.parsed_ok=1` was misleading: it meant transport/cache response succeeded, not that the chain step JSON parsed.
- Patch applied:
  - `chain_dispatch.rs` now invalidates the exact content-addressed cache row whenever chain/IR JSON parsing fails for primary, heal, or retry output.
  - `llm.rs` now sets audit `parsed_ok` from `extract_json(content).is_ok()` for `chain_dispatch`, `ir_dispatch`, and structured-response calls, including cache hits.
  - `db.rs::insert_llm_audit_cache_hit` now accepts caller-supplied `parsed_ok` instead of hardcoding true.
  - Regression tests added for exact cache-row invalidation and malformed chain-dispatch cache-hit audit rows.

## Runtime Test 2026-04-23: Chronicle Hid Decompose Start / Source Extract Skipped Market

- User test slug shown in UI: `architecturewalkerv3test12`; actual DB slug: `architecturewalkrev3test12`.
- Runtime evidence:
  - `source_extract` did not enter market. It skipped fleet with `no_fleet_peer`, then used OpenRouter `inception/mercury-2` for all 5 L0 items.
  - `enhance_question` and `decompose` did hit market through `gemma4:26b`.
  - `decompose` had backend `ChainStepStarted`, but the Chronicle hid it because `chain_step_started` was classified as mechanical and `Chronicle` hid mechanical rows once decision rows existed.
- Patch applied:
  - Active builds now show mechanical/background chronicle rows, so step-start/finish/cache rows stay visible during long steps.
  - Chronicle now maps `cache_miss`, `step_retry`, and `step_error`; retries/errors are visible decision rows.
  - Chain `when` and `from_depth` skip paths now emit `NodeSkipped` events instead of silently continuing.
  - Question decomposition LLM/audit events now inherit the actual chain step name (`decompose` / `decompose_delta`) when invoked by the chain executor, and question-node `NodeProduced` events derive the same step name from the decompose build id.

## Runtime Test 2026-04-23: First Step Fell Through To OpenRouter

- User test slug: `architecturewalkerv3test13`.
- Symptom: the first LLM step (`characterize`) used OpenRouter/Kimi even though market/gemma was configured and later source-extract market jobs worked.
- Agent audit confirmed this was not a missing `characterize` market slot:
  - `characterize` intentionally resolves Walker slot `max`.
  - Active `walker_provider_market` had `max: [gemma4:26b]`.
  - The Decision builder skipped Market before dispatch with `NoMarketOffersForSlot`.
- Root cause:
  - `MarketReadiness` treated a missing sync `walker_market_probe` per-model cache entry as "no market offers."
  - At boot, the first build decision can race ahead of the async market-surface poller/projector.
  - That contradicts the dispatch path, where `/quote` is the authoritative market viability check and cold cache is supposed to be permissive.
- Patch applied:
  - `MarketReadiness` now treats a missing per-model probe entry as `Ready`, keeping Market in `effective_call_order` so dispatch can quote.
  - `NoMarketOffersForSlot` remains for absent model lists and positive cached evidence that every declared model has `active_offers == 0`.
  - `walker_market_probe` design comments were updated to match the permissive cold-cache contract.
  - Added regressions for both direct readiness and `DispatchDecision::build("max", ...)` with active market config and cold probe cache.
- Validation:
  - `cargo test --lib pyramid::walker_readiness::tests::test_market_readiness_cold_model_cache_is_ready -- --nocapture` passed.
  - `cargo test --test walker_v3_market_readiness -- --nocapture` passed: 7/7.

## Runtime Test 2026-04-23: Test14 Market Label / Quote Expiry

- User test slug: `architecturewalkerv3test14`.
- Runtime evidence:
  - `characterize` now used market/gemma, so the cold-cache readiness fix worked.
  - `Q-L0-000` genuinely fell back to OpenRouter/Mercury after a market/gemma quote expired before purchase.
  - `Q-L0-001` through `Q-L0-004` used market/gemma.
  - Market failure chronicle rows (`network_quote_expired`, `network_route_skipped`) were mislabeled with the step-context/OpenRouter model when their metadata omitted a market `model_id`.
- Patch applied:
  - Market failure/retry chronicle emits now stamp the actual market model id (`market_model_id`) so skipped/expired market rows no longer inherit the OpenRouter candidate label.
  - `quote_jwt_expired` / `quote_expired` now retry the same market entry up to `retry_http_count` before cascading, instead of falling through to OpenRouter on the first expired quote.
  - Added a small helper regression for quote-expired slug classification.
- Validation:
  - `cargo test --lib pyramid::llm::tests::quote_expired_slugs_retry_same_market -- --nocapture` passed.
  - `cargo check` passed with existing warning noise.

## Runtime Test 2026-04-23: Test14 Final Failure / DB Lock

- User reported test14 eventually failed after the evidence loop had been running for ~36 minutes.
- DB evidence:
  - `pyramid_builds.slug`: `architecturewalkerv3test14`
  - `build_id`: `qb-129b33bd`
  - `status`: `failed`
  - `error_message`: `Chain aborted at step 'evidence_loop': failed to save answered node L1-000: database is locked`
  - `completed_at`: `2026-04-24 05:47:33` UTC / `2026-04-23 22:47:33` local.
- Compute chronicle evidence:
  - Evidence answers were successfully returning from the market as `gemma4:26b`.
  - Many rows were `network_quoted` -> `network_purchased` -> `walker_resolved` for `evidence_answer_batch_0`.
  - The last observed answer returned with `latency_ms=401931` (about 6m42s) and then the chain failed while saving `L1-000`.
- Interpretation:
  - The market path was working.
  - The failure was local persistence/locking during evidence answer materialization, not a market-routing failure.
  - This is probably related to the evidence loop answering multiple questions and then trying to persist while other read/write activity is active. The next debugger pass should inspect the evidence-loop save path in `chain_executor.rs` around answered-node persistence and DB locking strategy.
  - The UI showed `22/17 steps`, so progress accounting can overcount internal evidence-loop calls relative to top-level chain steps.

## Maximal Question DAG Work: Pre-Compaction Checkpoint

User direction changed from fixing the current hardcoded evidence loop to making question decomposition the maximal solution.

Target model:

```text
apex question
  -> canonical layer of subquestions
    -> canonical layer of subsubquestions
      -> evidence / answer material attaches to the stable question node
```

Important semantics:

- The question graph should be a DAG, not a tree.
- A canonical child question should be created once and connected to every higher-order parent question it helps answer.
- Generation should be layer-wise/frontier-based, not recursive depth-first branch completion.
- Dedupe/review should operate over the whole layer/frontier, not only among siblings under one parent.
- Answer/evidence/gap/provenance should accrue onto stable question identities via `source_question_id`.

Agent audits completed:

- Backend explorer verdict: current decomposition is still recursive/depth-first tree-shaped. Highest-risk functions are `decompose_question_incremental`, `build_subtree_incremental`, `save_tree_nodes_to_db`, `assign_question_ids`, `extract_layer_questions`, `decompose_question_delta`, and DB helpers around `pyramid_question_nodes`.
- Frontend/read-model explorer verdict: the surface can already render DAG edges if it receives explicit edges. The backend/read-model and inspector remain single-parent flavored. Practical fix is to add `parent_ids` and canonical edge adjacency, keep `parent_id` as legacy/default, and update inspector navigation to understand multiple parents.

Mid-patch source changes already made before this checkpoint:

- Added `parent_ids: Vec<String>` to:
  - `TreeNode`
  - `QuestionNodeDetail`
  - `LiveNodeInfo`
- Added canonical question edge schema in `db.rs`:
  - `pyramid_question_edges(slug, build_id, parent_question_id, child_question_id, edge_kind, ordinal, created_at)`
  - Primary key: `(slug, build_id, parent_question_id, child_question_id)`
  - Indexes on parent and child.
- Added DB helpers:
  - `QuestionEdgeRow`
  - `save_question_edges_for_parent`
  - `clear_question_edges`
  - `load_question_edges`
  - `question_edges_by_parent`
  - `question_edges_by_child`
  - `question_child_ids_for_row`
  - `question_parent_ids_for_row`
- Began converting read models:
  - `get_build_live_nodes` now loads canonical question edges, derives `parent_ids`, uses first parent only as legacy `parent_id`, and derives children from edge adjacency before falling back to `children_json`.
  - `query.rs` question projection now loads question edges and has a cycle guard for DAG projection.
  - `drill_question_node` now starts deriving `parent_ids` / children from question edges.

Very important mid-patch warning:

- This DAG patch is incomplete and has not been formatted, checked, or tested.
- The code may not compile yet.
- Finish the read-model conversion before attempting a build:
  - Add `parent_ids` to all remaining Rust `TreeNode` / `LiveNodeInfo` literals.
  - Verify `query.rs` function signatures/calls after the `question_node_detail` and `synthetic_question_node` changes.
  - Update TypeScript mirror types (`pyramid-surface`, theatre inspector) with `parent_ids`.
  - Update `flattenTree`, live-node merge, and inspector navigation to use all parents.
- Then implement the actual generator change:
  - Replace depth-first `build_subtree_incremental` path with a frontier/layer DAG builder.
  - Persist canonical edges whenever a layer is accepted.
  - Keep `pyramid_question_nodes.parent_id` / `children_json` as compatibility projection only.
  - Change question IDs away from depth-dependent identity if a shared semantic question can appear under multiple parents/depths.

No validation has been run after the DAG mid-patch edits.

## Re-Onboarding Check: Question DAG Mid-Patch

After compaction, the mid-patch was rechecked against the current worktree:

- `cargo check --bin wire-node-desktop` passes with existing warning noise.
- `npm run build` passes with the existing large chunk warning.
- Rust read models now compile with `parent_ids` on `TreeNode`, `QuestionNodeDetail`, and `LiveNodeInfo`.
- `pyramid_question_edges` schema and read helpers exist.
- `get_build_live_nodes`, `question_tree_projections`, and `drill_question_node` can derive question parents/children from canonical edges when edge rows exist.

Important semantic gap:

- No decomposition writer currently calls `save_question_edges_for_parent` or `clear_question_edges`.
- Fresh decomposition still runs through `decompose_question_incremental` -> `build_subtree_incremental`, which decomposes one branch recursively before moving to the next.
- Current horizontal review is still local to siblings under one parent, not a full frontier/layer review.
- The frontend TypeScript mirror still lacks `parent_ids` / `parentIds`, and surface/inspector navigation still use single `parent_id`.

So the branch is buildable, but the maximal DAG work is not implemented yet. The next implementation slice should be:

1. Finish `parent_ids` propagation through TypeScript surface and inspector types/navigation.
2. Persist compatibility edges for the existing tree path so the new edge read-model can be tested immediately.
3. Replace recursive branch decomposition with a layer-wise frontier DAG builder that creates canonical child questions once and writes every parent edge.

## Pre-Compaction Checkpoint: 2026-04-23 Late Session

Current user direction:

- Finish the maximal question DAG decomposition.
- Also own the reconciled SQLite/outbox fallout handed over from Claude Code.

Claude-side handoff context:

- Branch is still `walker-v3-shipping`.
- Claude commits now on top:
  - `878737a` — Agent 1 A-style fix: answered-node save retry + `BEGIN IMMEDIATE`.
  - `1f4ebba` — handoff doc.
  - `399a8a1` — Wave 7 hotfix.
- The outbox/B work is now ours to finish.

Outbox/B current state:

- Files involved by this lane:
  - `src-tauri/src/main.rs`
  - `src-tauri/src/pyramid/chain_executor.rs`
  - `src-tauri/src/pyramid/db.rs`
- `db.rs` has `pyramid_answered_node_outbox` schema and helpers:
  - `save_answered_node_outbox`
  - `get_pending_answered_node_outbox`
  - `mark_answered_node_outbox_drained`
  - `record_answered_node_outbox_error`
- `main.rs` configures the long-lived writer with `configure_pyramid_writer_connection`.
- `chain_executor.rs` stamps evidence links with `build_id`, writes answered material to the outbox before canonical save, reads it back for canonical drain, and records canonical-save errors on the outbox.
- Warning: after the last local reapplication of outbox usage in `chain_executor.rs`, validation has not yet been rerun. Run formatting/checks before trusting it.

Known test fallout to fix before committing:

- `cargo test --lib` moved from baseline `1967 pass / 15 fail` to `1983 pass / 17 fail`.
- New failures reported:
  - `pyramid::chain_dispatch::tests::test_w1b_build_step_dispatch_decision_empty_db_returns_some_default`
  - `pyramid::manifest::tests::test_hydrate_returns_node_content`
  - `pyramid::query::tests::test_drill_includes_web_edges_and_empty_when_no_thread`
- Likely cause from handoff: in-memory DB setup path around `init_pyramid_db` / new outbox DDL / writer pragma fixture expectations. Verify directly; do not assume.

Question DAG current state:

- First-class question-node surface is done and validated earlier:
  - YAML `decompose` / `decompose_delta` use `viz.type: node_fill`, `source: question_nodes`, `node_kind: question`.
  - Backend tree/live/drill read models expose question nodes.
  - Inspector opens question nodes and shows linked answer/evidence/gaps.
  - Answer nodes carry `source_question_id`.
- Maximal DAG is still not done:
  - `pyramid_question_edges` schema and Rust read helpers exist.
  - Rust read models have `parent_ids`.
  - But no decomposition writer calls `save_question_edges_for_parent`.
  - Generator is still recursive/depth-first through `decompose_question_incremental` -> `build_subtree_incremental`.
  - TypeScript still needs `parent_ids` / `parentIds` propagation in surface/inspector navigation.

Next order of operations:

1. Stabilize the SQLite/outbox lane:
   - Run targeted failing tests.
   - Fix only the regression source.
   - Run `cargo fmt`, targeted tests, and `cargo check --bin wire-node-desktop`.
2. Finish question DAG read-model/frontend:
   - Add `parent_ids` / `parentIds` to TS mirror types.
   - Make flatten/live merge/inspector navigation use all parents while preserving legacy `parent_id`.
   - Persist compatibility `pyramid_question_edges` for the existing tree path.
3. Implement maximal decomposition:
   - Replace fresh recursive branch decomposition with a frontier/layer DAG builder.
   - Persist apex first.
   - For each layer, present the full parent frontier to the LLM and ask for canonical children with `parent_ids`.
   - Deduplicate canonical child questions and write every parent edge.
   - Keep legacy `parent_id` / `children_json` as compatibility projections only.

## Worker A DAG Slice: 2026-04-24

Completed in this lane:

- Fresh incremental question decomposition now uses a frontier/layer DAG builder instead of recursively completing one branch before moving to the next.
- The new frontier LLM path asks for canonical child questions for the whole parent layer and accepts `parent_ids` / `parent_indices`, so one child can attach to multiple parents.
- Question IDs are now based on normalized question text rather than depth, so shared semantic questions can converge on one stable identity.
- `save_question_dag_to_db` persists `pyramid_question_nodes` plus canonical `pyramid_question_edges` together, while preserving legacy `parent_id` / `children_json` as a compatibility projection.
- The old tree save path now also writes compatibility question edges when it is used.
- Layer-question extraction and leaf collection de-dupe by question id so cloned shared nodes in the legacy tree projection do not produce duplicate answer work.
- Frontend surface/live-node/inspector types now carry `parent_ids` / `parentIds`, and layout plus inspector navigation honor all parents while keeping `parent_id` / `parentId` as the default legacy parent.

Validation:

- `cargo fmt`
- `cargo check --bin wire-node-desktop` passed with existing warning noise.
- `npm run build` passed with the existing large chunk warning.
- `cargo test --lib pyramid::question_decomposition::tests -- --nocapture` passed: 29/29.

Precise remaining TODO:

- Resume path still uses the old `get_undecomposed_nodes` + `build_subtree_incremental` branch repair flow. It now writes canonical edge rows as compatibility, but it is not a true frontier resume. If interrupted DAG builds need first-class resume semantics, add a DAG resume path that loads `pyramid_question_edges`, reconstructs the incomplete frontier by depth, and continues layer-wise.
- The fresh chain-executor build path is frontier-DAG now, but the preview helper `decompose_question` and delta path `decompose_question_delta` still use the older recursive/tree-shaped assembly. Convert those once the runtime DAG path is verified, or explicitly limit them to preview/legacy semantics.
- The frontier prompt relies on inline fallback unless `chains/prompts/question/decompose_frontier.md` is added. Add that prompt file if we want chain-authored wording instead of Rust fallback text.
- Runtime validation still needs a fresh build on a real slug to inspect actual `pyramid_question_edges`, multi-parent question rows, surface edges, and answer attachment through `source_question_id`.

## Stage Manager Integration: 2026-04-24

Integrated the two worker lanes and closed the audit-blocking DAG gaps found after their handback.

Question DAG changes after worker handback:

- Added `chains/prompts/question/decompose_frontier.md`, so the live frontier DAG prompt is now externalized instead of running from Rust fallback text.
- `call_frontier_decomposition_llm` now renders `{{min_subs}}` / `{{max_subs}}` for the frontier prompt and treats an empty parsed child list as malformed, causing retry/fallback rather than silently leafing a whole frontier.
- `decompose_question_incremental` no longer resumes via `build_subtree_incremental`. If question rows already exist for the slug, it reconstructs a `QuestionDagDraft` from `pyramid_question_nodes` plus canonical `pyramid_question_edges`, finds unfinished branch nodes as a same-depth frontier, and continues the same layer-wise DAG loop.
- Removed the now-dead recursive incremental resume helper path so fresh/resume runtime decomposition has one frontier-DAG writer.
- Added question-DAG unit coverage for resume-frontier reconstruction and multi-parent child preservation.
- Switched question DAG save transactions to `BEGIN IMMEDIATE`, matching the broader SQLite writer-lock fix direction.

Current residuals:

- Preview (`decompose_question`) remains a preview-only recursive tree builder.
- Delta (`decompose_question_delta`) remains a delta/reuse-specific tree-shaped helper used only when `load_prior_state.has_overlay == true`; it is not part of the clean fresh runtime path.
- YAML `instruction` still names the legacy decompose prompt while the Rust primitive selects `decompose_frontier.md` for frontier calls. The prompt text is externalized, but exact instruction-to-prompt selection is still primitive-coded.
- Runtime validation on a fresh real slug is still needed to inspect actual `pyramid_question_edges`, multi-parent rows, rendered surface edges, and answer attachment through `source_question_id`.

Validation:

- `cargo fmt --check`
- `git diff --check`
- `cargo check --bin wire-node-desktop` passes with existing warning noise.
- `cargo test --lib pyramid::question_decomposition::tests -- --nocapture` passes: 31/31.
- The three reported DB fallout tests pass after the final resume/prompt patch:
  - `pyramid::chain_dispatch::tests::test_w1b_build_step_dispatch_decision_empty_db_returns_some_default`
  - `pyramid::manifest::tests::test_hydrate_returns_node_content`
  - `pyramid::query::tests::test_drill_includes_web_edges_and_empty_when_no_thread`
- `answered_node_outbox` passes.
- `is_sqlite_busy` passes: 4/4.
- Final `npm run build` passes with the existing Vite large chunk warning.
