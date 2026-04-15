# Handoff — 2026-04-07 — chain-binding-v2.6 implementation ship

Implementation of `docs/plans/chain-binding-v2.6.md` (Sections 0-9). The wizard's "Chronological" option now runs the question pipeline with a forward → reverse → combine triple-pass L0 step, riding on a token-aware overlapping conversation chunker (~40k tokens with 6k overlap each side).

## TL;DR

The "Chronological" wizard option for Conversation slugs now does what the user wanted: runs forward+reverse+combine multi-pass L0 extraction, then continues through the full question pipeline (decompose → evidence_loop → process_gaps → l1/l2 webbing). v2.5's legacy intrinsic dispatch is unchanged and still serves vine bunches.

## Audit cycles run

- **Stage 1 informed** (auditors O, P): 2 CRITs + 9 HIGHs + several MEDs. Findings applied as `chain-binding-v2.6.md` Section 8.
- **Stage 2 discovery** (auditors Q, R, blind to Stage 1): 2 new CRITs + several HIGHs. Findings applied as Section 9. Stage 2 also independently confirmed/cleared many Stage 1 concerns.
- **No further audits** — Section 9.14 declared the gate met. Implementation followed the corrected plan.

## What shipped (v2.6)

### Path A — chain YAML + executor

| Item | File | Notes |
|---|---|---|
| New chain YAML | `chains/defaults/conversation-chronological.yaml` | content_type: conversation. Replaces source_extract with forward_pass + reverse_pass + combine_l0; copies refresh_state through l2_webbing verbatim from question.yaml |
| Forward prompt | `chains/prompts/conversation-chronological/forward.md` | Rewritten: Pillar 37 violations removed, explicit running_context integration instructions, chronological-aware schema |
| Reverse prompt | `chains/prompts/conversation-chronological/reverse.md` | Same. Walks chunks latest→earliest. |
| Combine prompt | `chains/prompts/conversation-chronological/combine.md` | **CRIT 9.1 fix**: rewritten to emit the question pipeline L0 contract `{headline, orientation, topics: [...]}` from `source_extract.md` so downstream l0_webbing/decompose/evidence_loop consume it. Chronological signals (turning_points, dead_ends, fate, corrections) folded into topic fields without breaking the schema. |
| `for_each_reverse: true` field | `chain_engine.rs:230`, `chain_executor.rs:5645` | Already existed from prior session. Validator at `chain_engine.rs:506-516` enforces `for_each_reverse` requires `for_each` AND `sequential: true`. |
| `$chunks_reversed` resolver | `chain_resolve.rs:102-106` | Already existed. Both approaches work; the YAML uses `for_each_reverse` (validator-enforced sequential is the simpler invocation). |

### Path B — token-aware chunker

| Item | File | Notes |
|---|---|---|
| `cl100k_bpe()` shared OnceLock accessor | `llm.rs:158-171` | New. Returns `Option<&'static CoreBPE>`. Single init point for both `count_tokens_sync` and the chunker. Safe from blocking thread contexts only (8MB stack); doc warns about 2MB async worker thread overflow. |
| `count_tokens_sync()` | `llm.rs:173-180` | Refactored to use `cl100k_bpe()`. |
| `chunk_transcript_tokens()` | `ingest.rs:325-419` | **Hardened**: tail merge (absorbs trailing chunks < target/4), `snap_to_line_boundaries` so chunks never split mid-speaker-label, FULL fallback to legacy `chunk_transcript` on any decode error (no silent partial recovery), uses shared OnceLock BPE. |
| `snap_to_line_boundaries()` | `ingest.rs:421-453` | New. Trims leading/trailing partial lines at byte-level (newline is ASCII so byte slicing produces valid UTF-8). First chunk's leading edge and last chunk's trailing edge preserved. |
| `Tier2Config.chunk_target_tokens` (28000) + `chunk_overlap_tokens` (6000) | `mod.rs:343-348` | Already existed from prior session. |
| `ingest_conversation` wired to new chunker | `ingest.rs:540-545` | `ingest_continuation` also wired. |

### Wizard wiring (already done in prior session, verified)

| Item | File | Notes |
|---|---|---|
| `conversationChain` state | `AddWorkspace.tsx:50` | Type `'question-pipeline' \| 'conversation-chronological'` |
| Dropdown option | `AddWorkspace.tsx:782` | `<option value="conversation-chronological">Chronological (forward + reverse + combine)</option>` |
| Persist on slug create | `AddWorkspace.tsx:295-301` | Always calls `pyramid_assign_chain_to_slug` for conversation slugs |

### Dispatch route (verified, not changed)

`build_runner.rs:245-302` is the canonical conversation dispatcher. The new chain id `conversation-chronological` ≠ `CHRONOLOGICAL_CHAIN_ID` (= `conversation-legacy-chronological`), so the legacy intrinsic fork at `:253` is bypassed and the slug falls through to `run_decomposed_build` at `:331`, which loads the assigned YAML chain via `chain_loader::load_chain` and executes through the chain executor.

`question_build.rs:226-335` has a duplicate WS-C dispatch hijack but it ALSO only checks `CHRONOLOGICAL_CHAIN_ID`, so the new chain id is unaffected on every entry path.

`ingest_conversation` callers verified inside `spawn_blocking` at `main.rs:3855`, `routes.rs:2380`, `vine.rs:867` — tiktoken stack-overflow risk neutralized for Path B.

`pyramid_create_slug` for conversations carries an apex_question via the fallback at `build_runner.rs:320-326` (default conversation question if not user-provided) — `enhance_question`/`decompose` work end-to-end.

## What is NOT shipped (deferred, non-blocking for smoke test)

Tracked in `docs/plans/chain-binding-v2.6.md` Sections 8+9.

| Deferred item | Plan ref | Why safe to defer |
|---|---|---|
| Backend `pyramid_assign_chain_to_slug` content_type guard | §8.8b | The wizard UI already only offers the Chronological dropdown for Conversation slugs. Backend defense is belt-and-suspenders. |
| Chunk-count-shrink cleanup of orphaned `pyramid_pipeline_steps` rows | §8.9 | Only triggers when the operator changes `chunk_target_tokens` between builds — no UI for that yet. Operator-tunable chunk size is a follow-up. |
| Invariant comment update at `chain_executor.rs:4072` | §9.11 | Pure forensic-clarity comment fix. Today safe because reverse_pass is `step_only` (no nodes saved). Future reverse-iterating step that saves nodes would need this fix. |
| `db.rs:856 updated_at` ALTER permanent fix | latent | v2.5 migration introspection works around it. Permanent fix is `DEFAULT CURRENT_TIMESTAMP` instead of `(datetime('now'))`. |

## How the new chain runs end-to-end

1. **Wizard**: pick Conversation slug, enter question, pick `Chronological` dropdown.
2. Slug created → `pyramid_assign_chain_to_slug(slug, "conversation-chronological")` writes the override row.
3. **Ingest**: `chunk_transcript_tokens` produces overlapping ~40k-token chunks, snapped to line boundaries, with tail merge.
4. **Build IPC** → `run_build_from` → conversation dispatcher → chain id ≠ legacy → `run_decomposed_build` → loads `conversation-chronological.yaml`.
5. **Phase 0**: `load_prior_state` reads any prior build state.
6. **Phase 1A** `forward_pass`: sequential `for_each: $chunks`, accumulates `running_context`. Each chunk's LLM call gets the rolling forward context. Outputs live in `ctx.step_outputs["forward_pass"]` (step_only).
7. **Phase 1B** `reverse_pass`: sequential `for_each: $chunks` with `for_each_reverse: true`, accumulates `running_context` walking backward. Items keep their original `index` field through reversal, so resume keys are stable.
8. **Phase 1C** `combine_l0`: sequential `for_each: $chunks`, uses `zip_steps` to pull `forward_pass[i]` and `reverse_pass[N-1-i]`. Sequential is mandatory (§8.3) — `current_index` propagation under concurrent dispatch is unverified. Outputs the question pipeline L0 contract; saved as `Q-L0-{index:03}` nodes at depth 0. **Gated by `when: l0_count == 0`** so it skips on resume after L0 already exists. forward_pass and reverse_pass are deliberately NOT gated (§9.2): resume after partial-combine needs them to repopulate `ctx.step_outputs`.
9. **Phase 2** `l0_webbing`: reads `$combine_l0` instead of `$source_extract`. Otherwise identical to question.yaml.
10. **Phases 3-6**: identical to question.yaml — refresh_state, enhance_question, decompose / decompose_delta, extraction_schema, evidence_loop, gap_processing, l1_webbing, l2_webbing.

## Risks not yet stress-tested

These are areas the audits flagged that the smoke test should specifically exercise:

1. **`extract` primitive `input:` keys → prompt JSON payload**: The plan assumes `chunk: "$item.content"`, `chunk_index: "$item.index"`, etc. flow into the LLM's user prompt as a serialized JSON dict (per `chain_dispatch.rs:193-194`). Verified for the `extract` primitive's general path. The smoke test should confirm forward.md sees a JSON like `{chunk: "...", chunk_index: 5, running_context: "..."}` in the LLM call logs.
2. **Accumulator REPLACE semantics** (§9.3): `update_accumulators` at `chain_executor.rs:6998` does `insert(name, truncated)` — overwrite, not append. The forward.md prompt explicitly tells the LLM to "Rewrite the prior running_context to fold in what just happened. Do not drop earlier context." If the LLM doesn't comply, running_context becomes one-chunk-memory. Watch the running_context evolution across chunks in the build logs.
3. **Single-chunk fast path** (§9.6): For transcripts that fit in one ~28k-token chunk, the chunker emits a single giant chunk. Re-ingestion after appending one new message flips the whole-file content_hash and Phase 3.4 invalidation nukes the entire prior build. Acceptable for now; operator-tunable chunk size is a follow-up.
4. **`combine_l0` zip_steps under concurrent dispatch** (§8.3): forced sequential to side-step the unverified `current_index` propagation. If you ever bump `concurrency > 1` on combine_l0, verify or fix.

## Smoke test procedure

1. Open the new build (release binary). Restart fully if a prior version is running.
2. Create a new Conversation slug via the wizard:
   - Pick a multi-conversation `.jsonl` file (or directory of them).
   - Enter your apex question.
   - Pick `Chronological` from the build pipeline dropdown.
   - Create.
3. Click Build.
4. Watch the build progress UI. You should see step names: `load_prior_state` → `forward_pass` (sequential, one chunk at a time) → `reverse_pass` (sequential, walking backward) → `combine_l0` (sequential, one chunk) → `l0_webbing` → `refresh_state` → `enhance_question` → `decompose` → `extraction_schema` → `evidence_loop` → `gap_processing` → `l1_webbing` → `l2_webbing`.
5. Verify L0 nodes have the question pipeline shape: open the pyramid drawer, check that L0 nodes show `headline`, `orientation`, and a `topics` array with `name`/`summary`/`current` fields (not the chronological-flat shape from v2.5's intrinsic).
6. Verify chunks are ~40k tokens with overlap: query `SELECT chunk_index, length(content) FROM pyramid_chunks WHERE slug = '<slug>'`. Expect ~150-200KB per chunk.
7. Resume test: cancel mid-`forward_pass`, restart build. Expect resume to skip completed chunks and continue.
8. Re-build test: rebuild the same slug. Expect Phase 3.4 hash invalidation to short-circuit unchanged chunks.

## File inventory (v2.6 changes)

- `docs/plans/chain-binding-v2.6.md` — full plan + Sections 8+9 audit corrections
- `docs/handoffs/handoff-2026-04-07-chain-binding-v2.6-and-vine-phase24.md` — context-loss recovery doc
- `docs/handoffs/handoff-2026-04-07-chain-binding-v2.6-implementation.md` — this file
- `chains/defaults/conversation-chronological.yaml` — new chain
- `chains/prompts/conversation-chronological/{forward,reverse,combine}.md` — rewritten
- `src-tauri/src/pyramid/llm.rs` — `cl100k_bpe()` accessor + refactored `count_tokens_sync`
- `src-tauri/src/pyramid/ingest.rs` — `chunk_transcript_tokens` hardened, `snap_to_line_boundaries` added, conversation ingest wired
- `src-tauri/src/pyramid/mod.rs` — `Tier2Config.chunk_target_tokens`/`chunk_overlap_tokens` (prior session)
- `src-tauri/src/pyramid/chain_engine.rs` — `for_each_reverse` field + validator (prior session)
- `src-tauri/src/pyramid/chain_executor.rs` — reverse iteration + `chunk_index` resume (prior session)
- `src-tauri/src/pyramid/chain_resolve.rs` — `$chunks_reversed` resolver (prior session)
- `src/components/AddWorkspace.tsx` — wizard dropdown updated to new chain id (prior session)

## What's queued after the smoke test passes

1. **recursive-vine-v2 Phase 2** (recursive ask escalation) — re-audit `docs/plans/recursive-vine-v2-phase-2-and-4-prep-v2.md` first, then implement.
2. **recursive-vine-v2 Phase 4-local** (cross-operator vines without payment) — same prep doc.
3. **Wizard Domain Vine UI** — backend ready, frontend follow-up.
4. **chain-binding-v2.5 Phase 5 docs tree** at `docs/chain-development/`.
5. **[BLOCKED]** Phase 4-paid — needs WS-ONLINE-H on the GoodNewsEveryone repo.

Plus the deferred v2.6 cleanups (content_type backend guard, orphan cleanup, invariant comment, latent updated_at fix).
