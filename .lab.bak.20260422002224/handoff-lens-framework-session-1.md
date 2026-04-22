# Handoff: Lens Framework Research — Session 1

## What We Were Doing
Researcher lab to find a generalized prompt framework for question pyramids that maximizes marginal usefulness at each layer, replacing the prescriptive 4-lens framework (Pillar 37 violation). Test corpus: 34 architecture docs from Core Selected Docs.

## What Happened
1. Set up lab on `research/lens-framework` (forked from `main`)
2. Discovered `main` is missing the entire chain engine, question pipeline, and ~30 modules — all live exclusively on `research/question-pyramid-tuning`
3. The running binary (v0.2.0) is compiled from `research/question-pyramid-tuning`, so the API works even though our branch doesn't have the code
4. Got auth working (token must be set via UI, not disk — app overwrites config on shutdown)
5. Ran baseline build on slug `lens-0` — **build completed but pyramid is broken**

## Baseline Build Results (lens-0)
- **34 L0** nodes extracted (good)
- **7 W-L1** mechanical web domain nodes created during webbing phase (good)
- **1 L1** question-answer node created: "What are the core subsystems..." with 8 KEEP verdicts
- **1 L2** node created: "How has the system evolved across iterations?" — DISCONNECTED, no evidence
- **No apex**
- Build took 23 minutes, 54 steps, 0 failures reported

### The Problem
The decompose step generated a rich question tree: 5 L2 branches × ~5 leaves each = 25 leaf questions. All stored correctly in `pyramid_question_tree` and `pyramid_question_nodes`. But the evidence loop only answered **1 of 25 leaf questions** and created **1 of 5 branch nodes**. The other 24 leaf questions and 4 branch questions produced no pyramid nodes.

The question tree is fully formed. The evidence loop just didn't process most of it. This is a chain executor / evidence_loop issue, not a prompt issue. The lens framework research can't proceed until builds actually produce complete pyramids.

### Key DB Evidence
```
pyramid_question_nodes: 31 rows (full tree)
pyramid_nodes WHERE depth>0: 9 rows (7 web + 1 L1 + 1 L2)
pyramid_evidence: 9 rows (8 KEEP + 1 DISCONNECT)
pyramid_cost_log: 0 rows (chain executor doesn't log here)
pyramid_pipeline_steps: 50 rows (none for evidence_loop/decompose — chain executor logs separately or not at all)
```

## Branch Situation — CRITICAL
`main` is massively behind. The entire chain engine lives only on `research/question-pyramid-tuning`. This needs to be merged to main before any further research branches.

### Merge Plan
1. **Merge `research/question-pyramid-tuning` → `main`** — this is the SOTA. All chain executor code, question pipeline, IR executor, build_runner, routes with `/build/question` endpoint, everything.
2. **Rebase `research/lens-framework` onto new `main`** — the lens-framework branch only has prompt file changes (17 .md files added to repo + .gitignore update). Easy rebase.
3. **Verify** — after rebase, `research/lens-framework` should have both the chain engine AND the prompt files tracked in git.

```bash
# Suggested sequence
git checkout main
git merge research/question-pyramid-tuning
git checkout research/lens-framework
git rebase main
```

Watch for conflicts in `chains/prompts/question/` — the tuning branch modified prompts, and lens-framework added them from the runtime copy. Take the lens-framework versions (they're the latest runtime state with 4-lens prompts).

## Evidence Loop Investigation
After branches are clean, the next session needs to investigate why the evidence loop only processed 1/25 questions. Key files to examine (all on `research/question-pyramid-tuning`):

- `src-tauri/src/pyramid/evidence_answering.rs` — the core evidence loop logic
- `src-tauri/src/pyramid/chain_executor.rs` — how `evidence_loop` primitive dispatches
- `src-tauri/src/pyramid/chain_dispatch.rs` — how questions get routed to the answerer

Questions to answer:
1. Does the evidence loop iterate over `pyramid_question_nodes` or `pyramid_question_tree`?
2. Is there a filter that skips questions when W-L1 web nodes already exist at that depth?
3. Are the leaf questions (depth=2, `is_leaf=true`) being skipped because max_depth=3 and the executor thinks they're too deep?
4. Is there a concurrency or batching issue where the loop processes one question and then exits?

## Lab State
- `.lab/` is set up at `/Users/adamlevine/AI Project Files/agent-wire-node/.lab/`
- Config, rubric (MMU — maximal marginal usefulness), results.tsv all initialized
- No experiments logged yet (baseline build was broken)
- Slug `lens-0` exists with 34 ingested docs and a broken pyramid
- All 17 prompts committed to `chains/prompts/question/` on `research/lens-framework`

## Skill Updates Made
- `wire-pyramid-ops/SKILL.md` — updated with correct `/build/question` endpoint, auth token fix instructions, CLI access patterns, correct DB path
- Added reference to `CHAIN-DEVELOPER-GUIDE.md` in runtime chains directory

## Key Decisions / Context
- **Primary model:** `minimax/minimax-m2.7` (set in UI settings, confirmed in pyramid_config.json)
- **Pillar 37:** The 4-lens framework violates it. "Four lenses" is a number constraining LLM output.
- **The pyramid is a scaffold, not a final product.** Default altitude should be ground truth, not developer truth. Technical depth comes on-demand via follow-up questions, not baked into the initial framework.
- **Maximal marginal usefulness** = each layer adds genuinely new understanding beyond the layer below. The rubric is in `.lab/config.md`.
- **Auth token chicken-and-egg:** The `/pyramid/config` endpoint is auth-protected, so you can't set the token via API when empty. Must use UI Settings panel. App overwrites `pyramid_config.json` on shutdown with in-memory state.

## Priority Order for Next Session
1. Merge branches (research/question-pyramid-tuning → main, rebase lens-framework)
2. Investigate evidence loop — why only 1/25 questions answered
3. Fix the evidence loop issue (this IS a Rust change — hand off the specific fix)
4. Re-run baseline build on architecture docs
5. Resume researcher lab from Phase 3
