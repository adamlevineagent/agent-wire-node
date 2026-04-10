# Handoff — 2026-04-09 Session 3 — Vine Episodic Memory + Evidence Mode

## TL;DR

Three-gap vine fix shipped and verified. Anti-confabulation prompt shipped and verified. Evidence mode infrastructure built (expression engine, build_lifecycle primitive, chain registry routing, vine threading). Fast mode chain created but provides negligible speedup (~13 min vs 14 min deep) — the real bottleneck is L0 extraction (63 LLM calls per bunch), not evidence grounding (~15 calls). Fast mode is shelved as infrastructure-ready but not worth using yet.

## What shipped (all committed, pushed, verified)

### Three-gap vine fix (commit 9cd5291)
- WS-0: Vine L0 assembly copies episodic fields (4 locations: assemble_vine_l0 apex/penultimate + notify_vine_of_bunch_change apex/penultimate)
- WS-1: Vine L1/upper uses synthesize_recursive.md + build_node_from_output (not legacy DISTILL_PROMPT + node_from_analysis). Includes distilled backfill from narrative, episodic_child_payload_json, include_str! prompt, chain_dispatch import, forced apex fallback with episodic fields + char-safe truncation, speaker field on KeyQuote, weight as object in payloads
- WS-2: Vocabulary refresh after vine builds (handle_vine_build Ok arm)
- WS-3: Nav page auth (pyramid_get_auth_token IPC, auth headers on all fetches, authToken in all 3 useEffect deps), VocabEntry interface matched to backend (name/liveness not canonical_name/live), vocab response flatten, thread wildcard (show_all), matched_field→matched_text, d.decided not d.question, r.node_id not r.id, MemoirView narrative rendering, AddWorkspace auth, empty auth_token localhost bypass

### Anti-confabulation prompt (commit 9363fb5)
- GROUNDING section in synthesize_recursive.md: no time inference, no dramatization, no significance inflation, every claim traces to input, clinical tone, descriptive headlines
- Verified on v6 vine: "The combined material documents three consecutive phases" vs prior "Across two years the team moved from a blank-slate start"

### Evidence mode infrastructure
- Expression engine: string literal parsing + string/boolean comparison (commit 58879b8)
- evaluate_when delegates to expression engine (same commit)
- build_lifecycle primitive: overlay cleanup extracted from evidence_loop (commit c2eb275)
- evidence_mode threaded through vine path: handle_vine_build → build_vine → build_all_bunches → build_bunch → run_build_from_with_evidence_mode (commit 7cfeae4)
- API exposure: VineBuildBody.evidence_mode + QuestionBuildBody.evidence_mode (commits 0e64bb7, 7cfeae4)
- Chain registry: default_chain_id_for_mode selects chain by evidence_mode (commit c301eba)
- conversation-episodic-fast.yaml: L0 extraction → recursive synthesis directly, no evidence (commit c301eba)
- Bunch slug collision fix: increment index until non-colliding (commit 9505ac7)

### Other fixes
- AddWorkspace.tsx auth headers (commit 9964706)
- Empty auth_token bypass for localhost (commit 9964706)
- VocabEntry.importance null-safe access (commit ab78349)

## Test results

| Vine | Mode | Time | Nodes | Apex narrative | Entities | Decisions |
|---|---|---|---|---|---|---|
| v5 (pre-prompt-fix) | deep | 14 min | 12, depth 2 | "Over two years..." (confabulated) | 13 | 7 |
| v6 (anti-confab) | deep | 14 min | 10, depth 1 | Grounded, factual, session-referenced | 7 | 17 |
| v10 (fast chain) | fast | 13 min | 12, depth 2 | Grounded, comparable quality | 0 | 8 |

Fast mode saves ~1 minute — not meaningful. The L0 extraction phase (forward+reverse+combine = 63 calls per bunch) dominates build time. Evidence grounding (~15 calls) is a small fraction.

## What fast mode needs to actually be fast

The bottleneck is L0 extraction, not evidence. To make fast mode meaningful:
1. **Single-pass L0 extraction** instead of forward+reverse+combine (3x reduction in L0 calls)
2. **Chunk batching** — process multiple chunks per LLM call instead of 1:1
3. **Parallel bunch builds** — currently sequential to avoid write conflicts

These are chain architecture changes, not evidence_mode toggles.

## Architecture decisions made this session

1. **Fast mode = separate chain YAML, not when-clause skip.** Different build strategies need different step sequences, not the same chain with conditional skips. When clauses are for delta/resume logic, not mode selection.
2. **build_lifecycle extracted from evidence_loop.** Overlay cleanup (L1+ node superseding) must run regardless of whether evidence is computed. It's now its own primitive.
3. **evaluate_when delegates to expression engine.** The legacy hand-rolled comparisons in evaluate_when silently coerced strings to 0.0. Now delegates to the expression engine for anything with operators.
4. **Anti-confabulation is a prompt concern, not a code concern.** Mercury 2 confabulates timescales and dramatizes unless explicitly told not to. The fix is guardrails in the prompt, not filtering in code.

## Known issues (from audit cycles, catalogued in plan)

### Follow-up workstreams (in docs/plans/vine-episodic-three-gap-fix.md)
- WS-F1: ts-rs type generation (eliminate frontend/backend type drift)
- WS-F2: Chain executor dispatch_pair episodic fix (same child_payload_json issue in per-session builds)
- WS-F3: Watcher new-bunch path broken (CRITICAL for live mode)
- WS-F4: Bug fix & tech debt sweep (Decision struct enrichment, parse_date_gap, chunk_index gaps, MemoirView enrichment, DADBEAR polling, reading modes scaling, dead code cleanup, View Vine tab crash)

### Pre-existing test failures (unchanged, 7 of 785)
- staleness test fixtures (5)
- evidence PK cross-slug (1)
- YAML schema response (1)

## Git log (this session)
```
27238ea fix: fast chain build_lifecycle references $load_prior_state not $refresh_state
c301eba feat: fast mode as separate chain YAML, not when-clause skip
9505ac7 fix: increment bunch slug index to avoid chunk collision
ab78349 fix: null-safe VocabEntry.importance access in nav page
7cfeae4 feat: thread evidence_mode through vine build path
0e64bb7 feat: expose evidence_mode in question build API
c2eb275 feat: build_lifecycle primitive + evidence_mode when guards (proper extraction)
58879b8 feat: evidence_mode support — string comparison in expression engine + skip path
9363fb5 fix: anti-confabulation guardrails in synthesize_recursive.md
9964706 fix: AddWorkspace auth headers + empty auth_token bypass for localhost
9cd5291 feat: vine episodic memory — three-gap fix (MPS-audited, 2-cycle audit)
e26ac68 docs: vine episodic three-gap fix plan (MPS-audited, 2-cycle audit)
```

## Key learning

**"Ship right not fast" applies to session timing too.** When the human steps away, don't treat it as a deadline. Continue working the plan. A pragmatic shortcut that keeps concerns coupled (the early-return hack inside evidence_loop) cost more time to fix than doing the proper extraction would have taken. The plan is the commitment, not the timeline.
