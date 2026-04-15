# Handoff — 2026-04-07 — chain-binding-v2.6 + recursive-vine Phase 2/4

Robust pickup doc. Assume the next session has zero memory of this one. Everything load-bearing must be here, not in chat history.

## Where we are

- **chain-binding-v2.5** shipped (wizard dropdown + override layer + WS-C dispatch fix in `spawn_question_build`). Postmortem: `docs/handoffs/handoff-2026-04-07-chain-binding-v2.5-postmortem.md`.
- **v2.5 was architecturally wrong**: the "Chronological" wizard option dispatches to legacy Rust intrinsic `build_conversation` (vine bunch path), bypassing the question pipeline entirely. User wanted question-pipeline-WITH-chronological-L0, not a separate intrinsic.
- **chain-binding-v2.6 plan written** at `docs/plans/chain-binding-v2.6.md`. Stage 1 informed audit (auditors O, P) is **in flight**. Stage 2 discovery audit (Q, R) is queued after.
- **Baseline test of v2.5 chronological intrinsic was abandoned** by user ("I dont care about this baseline, let's move forward"). No baseline data captured.

## v2.6 plan in one paragraph

Path A: new chain `chains/defaults/conversation-chronological.yaml` in legacy ChainStep DSL. Replaces `question.yaml`'s single `source_extract` step with three steps (forward / reverse / combine) using existing primitives — sequential `for_each` + `accumulate` for forward, `zip_steps` with `reverse: true` + new `for_each_reverse: bool` field for reverse, then `save_as: step_only` to fuse into the existing pipeline contract. The rest of the question pipeline (refresh_state, enhance_question, decompose, evidence_loop, process_gaps, l1_webbing, l2_webbing) is **copy-pasted** from `question.yaml`. Wizard dropdown swaps chain id from `conversation-legacy-chronological` (v2.5, dispatches to intrinsic) to `conversation-chronological` (v2.6, falls through to default question pipeline path in `spawn_question_build`).

Path B: token-aware overlapping chunker for `ingest_conversation`. New `chunk_transcript_tokens` function in `src-tauri/src/pyramid/ingest.rs` using `count_tokens_sync` (new public sync helper in `llm.rs`, wrapping the existing tiktoken `OnceLock<CoreBPE>`). Layout: 28k unique tokens forward + 6k overlap each side = 40k total per chunk (15%/70%/15%). New `Tier2Config` fields `chunk_target_tokens: 28000`, `chunk_overlap_tokens: 6000`.

## Decisions captured (in case plan/postmortem get lost)

1. **`build_conversation` legacy intrinsic stays put** — vine bunches still need it. Don't delete.
2. **New chain id is `conversation-chronological`** (different from v2.5's `conversation-legacy-chronological`). Both coexist; wizard points at the new one.
3. **`for_each_reverse: bool` is the right field, not `dispatch_order`**. `dispatch_order` is a no-op (chain_executor.rs ~5640) — warning logged, value ignored. Don't try to reuse it.
4. **Existing prompts at `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` are reused after Pillar 37 sweep** — they currently violate Pillar 37 (prescribe output sizes like "10-20%", "1-3 sentences", "4-12 word recognizable name"). Sweep is part of v2.6 implementation.
5. **Build maximal, no deferral framing.** Same-day phase ships are normal. Don't use ship-it-safer language.
6. **Audit til clean.** Two stages (informed + discovery), source-grounded, before implementation. Read source files yourself; don't trust auditor or plan claims unverified.
7. **Free-string ContentType** shipped as part of v2.5 (`ContentType::Other(String)` with manual serde) — keeps content_types open.
8. **Pillar 37**: prompts must not prescribe output sizes/lengths/counts.

## Latent bugs (don't lose track)

- **`db.rs:856`** — `ALTER TABLE pyramid_slugs ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'))` silently fails on existing DBs (SQLite restriction: NOT NULL ADD COLUMN rejects expression defaults). v2.5 works around it via dynamic `PRAGMA table_info` introspection in `migrate_slugs_drop_check`. **Permanent fix**: drop the line OR switch to `DEFAULT CURRENT_TIMESTAMP`. Untouched on purpose during v2.5 to avoid scope creep.
- **`chunk_transcript` (`ingest.rs:262`) line-based boundaries** are insufficient for dense markdown / non-`--- [A-Z]` formatted content. Path B addresses for conversations only; documents/code unaffected.
- **`spawn_question_build` chronological dispatch** (`question_build.rs` ~226-335) is hardcoded to check chain id `CHRONOLOGICAL_CHAIN_ID` and route to `vine::run_build_pipeline`. v2.6's new chain id must NOT match this constant, or it'll get hijacked. **Verify in implementation.**
- **`question_build.rs` does not actually load YAML chains** — it calls `build_runner::run_decomposed_build` directly with hardcoded pipeline. **THIS IS A POTENTIAL CRIT FOR v2.6 PATH A.** The plan assumes the new YAML chain is invoked through the question pipeline path, but if `spawn_question_build` doesn't read chain YAML, Path A is broken. Auditors O/P are checking this. If confirmed, Path A needs a different mechanism: either route through `chain_executor` directly, or extend `run_decomposed_build` to load and run a chain id when one is bound.

## Plan inventory

| File | Status | Notes |
|---|---|---|
| `docs/plans/chain-binding-v2.6.md` | written, **in audit** | Path A + Path B |
| `docs/plans/recursive-vine-v2-phase-2-and-4-prep-v2.md` | written, audited (5 CRITs addressed in v2) | needs **re-audit before impl**; CRIT-5 = re-resolve walks referrer graph |
| `docs/handoffs/handoff-2026-04-07-chain-binding-v2.5-postmortem.md` | written | what shipped/deferred in v2.5 |
| `docs/plans/chain-binding-v2.5.md` | shipped | the override layer + wizard + dispatch fix |
| `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` | drafted | Pillar 37 sweep pending |

## Queue (in order)

1. **NOW**: Stage 1 audit O+P on v2.6 (running). Then Stage 2 audit Q+R. Then apply findings.
2. **Implement v2.6**: ChainStep `for_each_reverse`, executor reverse iteration, Pillar 37 sweep, `conversation-chronological.yaml`, wizard chain id swap, `count_tokens_sync` helper, `Tier2Config` token fields, `chunk_transcript_tokens`, wire into `ingest_conversation`. **PREREQUISITE**: resolve the spawn_question_build / YAML-chain question (see latent bug above).
3. **recursive-vine-v2 Phase 2** (recursive ask escalation) — re-audit prep doc v2 first.
4. **recursive-vine-v2 Phase 4-local** (cross-operator vines, no payment).
5. **Wizard Domain Vine UI** — backend ready, FE only.
6. **chain-binding-v2.5 Phase 5 docs** at `docs/chain-development/`.
7. **[BLOCKED cross-repo] Phase 4-paid** — needs WS-ONLINE-H on GoodNewsEveryone.

## Test plan for v2.6 (when impl lands)

- Wizard create conversation slug → pick Chronological → ingest a real multi-conversation transcript.
- Verify `pyramid_chain_assignments` row written with chain id `conversation-chronological`.
- Verify build runs through question pipeline (decompose, evidence_loop, webbing) — NOT through `build_conversation` intrinsic.
- Verify L0 nodes show forward/reverse/combine evidence shape.
- Verify chunks are ~40k tokens with overlap (inspect `pyramid_chunks` rows).
- Re-run with `from_depth > 0`: resume should work for L1+ but L0 should be skipped (or rebuilt, depending on plan resolution).

## Open questions for next session

- Does `spawn_question_build` load YAML chains? (auditors checking)
- If not, does v2.6 Path A reroute to `chain_executor::run_chain` or extend `run_decomposed_build`?
- Pillar 37 sweep: should running_context be unconstrained or bounded by token budget?
