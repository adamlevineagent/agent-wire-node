# Session Handoff — 2026-04-04

## What happened this session

Started debugging Mercury 2 runaway outputs (47K tokens from 5K input). Discovered the root cause (no per-step output budget, Mercury fills the 48K ceiling on certain docs). Pivoted from mechanical pipeline debugging to question pyramids when Adam realized question pyramids self-decompose and should be the general framework — mechanical pipelines are presets.

Key outcomes:
1. **Question pyramid prompts tuned** — enhance_question.md, decompose.md, horizontal_review.md, answer.md all rewritten. Baseline 1.50/10 → 8.80/10 across 4 experiments.
2. **Understanding web architecture doc written** — `docs/architecture/understanding-web.md`. Canonical design. Questions drive everything. Evidence accretes. DADBEAR keeps it current.
3. **Recipe-as-contribution plan written and built** — ~1200 lines of Rust recipe → 65-line YAML chain + 4 executor primitives. Implemented by a separate builder session.
4. **Wire pillars updated** — Pillar 18 (any number of chain definitions, one IR, one executor) and Pillar 26 (grapejuice is question-shaped L0 extraction).
5. **Full codebase audit** — 90 findings, 5 critical + ~30 major fixed. See `docs/handoff-2026-04-04-holistic-audit.md`.

## Current state

**Binary:** Post-recipe-as-contribution + holistic audit. Running and healthy.

**All slugs archived.** Zero active slugs, zero DADBEAR cost. Fresh slugs needed for testing.

**question.yaml is live** at `chains/defaults/question.yaml`. Steps: load_prior_state → enhance_question → decompose/decompose_delta → extraction_schema → l0_extract → evidence_loop → gap_processing.

## What's broken right now

Three YAML/prompt wiring issues found during testing on the new binary. All are integration bugs between the new question.yaml chain and the new Rust primitives:

### 1. extraction_schema step doesn't receive decomposed questions
The `extraction_schema` step in question.yaml had no `input` block — the LLM received no questions and generated a useless "no questions provided" extraction prompt. I added `input: { question_tree: "$decompose" }` but then hit issue #2.

### 2. $decompose_delta unresolved on fresh builds
The extraction_schema input referenced both `$decompose` and `$decompose_delta`. On fresh builds, `decompose_delta` never runs (its `when` condition is false — no existing overlay). The chain executor aborts on unresolved `$ref`. I removed `$decompose_delta` from extraction_schema's input. **But this means on delta builds, extraction_schema won't receive the delta tree.** The fix needs the executor to handle optional/nullable refs gracefully, or the YAML needs conditional input wiring.

### 3. L0 extraction produces markdown instead of JSON
The extraction_schema-generated extraction prompt doesn't include JSON output format instructions. Mercury responds with conversational markdown ("Here's a quick rundown...") instead of structured JSON. I edited `extraction_schema.md` to tell it to include JSON format in the generated prompt, but haven't confirmed the fix works.

## Prompt fixes applied (in repo AND runtime)

- **enhance_question.md** — Added JSON output wrapper: `Respond with ONLY a JSON object: {"enhanced_question": "..."}` + `/no_think`. Previously returned raw text which the extract primitive couldn't parse.
- **extraction_schema.md** — Added instruction for the generated extraction_prompt to include JSON output format and `/no_think`. Previously the generated prompt didn't tell Mercury to output JSON.
- **question.yaml** — Added `input` block to extraction_schema step. Removed `$decompose_delta` ref that crashed on fresh builds.

## What needs to happen next

### 1. Debug the YAML wiring (highest priority)
The three issues above need resolution. The builder who wrote the primitives knows exactly how they consume inputs — they should be able to fix the wiring quickly. Key question: how does the `extract` primitive pass `step.input` content to the LLM? Is it serialized as the user prompt? Appended to the instruction? The prompts need to match whatever the primitive does.

### 2. Fresh end-to-end test
Once wiring is fixed:
```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
# Code (small, fast feedback)
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/slugs -d '{"slug":"test-code","content_type":"code","source_path":"/Users/adamlevine/AI Project Files/vibesmithy"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/test-code/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/test-code/build/question -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'

# Docs (full test)
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/slugs -d '{"slug":"test-docs","content_type":"document","source_path":"/Users/adamlevine/AI Project Files/Core Selected Docs"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/test-docs/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/test-docs/build/question -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'
```

Verify: apex endpoint works, drill follows evidence links, self_prompt displayed, gap processing runs, targeted re-examinations produce L0 nodes.

### 3. Delta decomposition test
Run a second question on the same slug after the first completes. Verify it detects existing overlay, runs decompose_delta, cross-links existing answers.

### 4. Update chain developer guide
`docs/chain-system-reference.md` is stale — doesn't know about question.yaml, the 4 new primitives, instruction_from, mode field, or save_as: step_only. Needs a rewrite to cover the understanding web architecture.

## Key files

| File | What it is |
|------|-----------|
| `docs/architecture/understanding-web.md` | Canonical architecture — read this first |
| `docs/plans/recipe-as-contribution.md` | Implementation plan (status: IMPLEMENTED) |
| `docs/handoff-2026-04-04-holistic-audit.md` | Full audit results + fix list |
| `chains/defaults/question.yaml` | The forkable question pipeline recipe |
| `chains/prompts/question/*.md` | All question pipeline prompts (11 files) |
| `.lab/` | Research lab — experiment log, results, friction log, parking lot |
| `GoodNewsEveryone/docs/wire-pillars.md` | Updated pillars (18 and 26 changed) |
| `GoodNewsEveryone/docs/architecture/understanding-web.md` | Copy of architecture doc |

## Key memories for context

- `project_pyramid_system.md` — Knowledge Pyramid system architecture
- `feedback_pillar37_no_hedging.md` — No numbers constraining LLM output, ever
- `feedback_architectural_lens.md` — "Can an agent improve this?" If no, it's hardcoded and wrong
- `feedback_split_big_agents.md` — One agent per focused task
- `feedback_handoff_no_deferrals.md` — Every handoff item is required or shouldn't be there

## Remaining judgment items (from holistic audit)

7 items not fixed, documented in `docs/handoff-2026-04-04-holistic-audit.md` Part 4:
1. Pillar 37 across 8+ sites (Tier2Config::default bypassing operator config)
2. ModeRouter remounts (tab switch loses state)
3. Error response sanitization (internals leaked to remote callers)
4. Horizontal review index-shift (leaf marks point to wrong siblings after merges)
5. /auth/complete CSRF (missing nonce)
6. Tunnel token encrypted storage (plaintext on disk)
7. Shared reqwest::Client (44 separate instances)
