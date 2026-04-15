# Handoff: Question Pipeline Prompt Tuning

## What you're walking into

The question pipeline is built and working end-to-end. A fresh slug with 34 vibesmithy code files produced a 4-layer pyramid (34 L0 → 9 L1 → 7 L2 → 1 apex) in 72 seconds with zero failures. The architecture is sound. The prompts need tuning.

Read these before starting:
- `docs/chain-developer-guide-v2.md` — how the YAML/prompt system works, common tasks, troubleshooting
- `docs/question-pipeline-guide.md` — question pipeline specifics (canonical aliases, input wiring, forking)
- `docs/architecture/understanding-web.md` — the design philosophy

## Current quality

The prompts were tuned on a previous binary (experiments 0-4 in `.lab/`). The recipe-as-contribution refactor changed how inputs flow to prompts, so the tuning partially reset. The most recent successful build (vibe-test3, 34 code files) shows:

**What works:**
- Decomposition produces a tree with branches and leaves (not flat)
- Extraction_schema generates a question-shaped prompt
- L0 extraction uses the generated prompt (via instruction_from)
- Evidence loop answers questions from L0 evidence
- Apex exists and is reachable via drill
- Gap processing runs

**What needs improvement:**
- **Low evidence utilization**: 8/34 L0 nodes touched. The pre-mapper connected questions to only 8 evidence nodes. 26 L0 nodes were extracted but never referenced by any question. This means either: (a) the extraction produced evidence the questions don't need, or (b) the pre-mapper can't find the connection between evidence and questions.
- **Decomposition may be too narrow**: The tree should cover the major dimensions of the corpus. If it decomposes into only 3-4 branches, large parts of the codebase are invisible.
- **Extraction quality unknown**: The question-shaped extraction (via extraction_schema) is new — nobody has inspected what the generated extraction prompts actually look like or whether the resulting L0 nodes are useful.

## The prompts to tune (priority order)

### 1. `extraction_schema.md` — highest leverage

This prompt generates the extraction prompt that L0 uses. If this generates a bad prompt, everything downstream is bad. Current issue: the generated prompt may not be specific enough, or may not include proper JSON format instructions for the extractor.

**How to diagnose:** Run a build, then check what extraction_schema generated:
```bash
sqlite3 ~/Library/Application\ Support/wire-node/pyramid.db \
  "SELECT output_json FROM pyramid_pipeline_steps WHERE slug='YOUR-SLUG' AND step_type='extraction_schema';" \
  | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d.get('extraction_prompt','(none)'))"
```

If the extraction_prompt is vague ("extract important information") or missing JSON format instructions, this prompt needs work.

### 2. `pre_map.md` — evidence utilization

This prompt maps questions to candidate evidence nodes. It receives all questions + all L0 node summaries and returns candidate lists. Current issue: only 8/34 nodes mapped. It should over-include (false positives are cheap).

**How to diagnose:** Check how many L0 nodes were KEEP'd vs DISCONNECT'd:
```bash
sqlite3 ~/Library/Application\ Support/wire-node/pyramid.db \
  "SELECT verdict, count(*) FROM pyramid_evidence WHERE slug='YOUR-SLUG' GROUP BY verdict;"
```

If DISCONNECT >> KEEP, pre_map is over-including (correct behavior, answer step prunes). If total evidence links << total L0 nodes, pre_map is under-including (wrong — evidence is being missed).

### 3. `decompose.md` — decomposition quality

Controls how the apex question is broken into sub-questions. The prompt receives the question + source material summaries. Key tuning targets:
- Branch vs leaf decisions: broad areas should be branches (they get their own answer nodes), specific questions should be leaves
- Coverage: sub-questions should address the major dimensions visible in the source material, not just surface-level categories
- Depth: decomposition should go deep enough that leaf questions are answerable from a focused set of evidence

### 4. `answer.md` — synthesis quality

Controls how evidence is synthesized into answers. Key tuning targets:
- KEEP/DISCONNECT/MISSING verdicts should be accurate
- All distinct dimensions of the answer should be reflected (not just the "strongest" evidence)
- Answers should be dense and specific — names, decisions, relationships from the evidence

### 5. `enhance_question.md` — question expansion

Turns a brief user question into a corpus-specific comprehensive question. Current version works but could be sharper at identifying the major dimensions from sample headlines.

## How to test

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"

# ALWAYS use fresh slugs. Never reuse old ones — stale state confuses everything.
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/slugs \
  -d '{"slug":"tune-1","content_type":"code","source_path":"/Users/adamlevine/AI Project Files/vibesmithy"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/tune-1/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/tune-1/build/question \
  -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'

# Poll
curl -s -H "$AUTH" localhost:8765/pyramid/tune-1/build/status

# Inspect
sqlite3 ~/Library/Application\ Support/wire-node/pyramid.db \
  "SELECT depth, count(*) FROM pyramid_nodes WHERE slug='tune-1' AND superseded_by IS NULL GROUP BY depth;"
```

**After EVERY prompt edit:** Sync to runtime before building:
```bash
SRC="/Users/adamlevine/AI Project Files/agent-wire-node/chains/prompts/question"
DST=~/Library/Application\ Support/wire-node/chains/prompts/question
for f in "$SRC"/*.md; do cp "$f" "$DST/$(basename "$f")"; done
```

## Tuning on vibesmithy (small) first

Vibesmithy is 34 files, builds in ~70 seconds. Iterate here until quality is good, THEN test on the 127-doc Core Selected Docs corpus. The doc build takes 5+ minutes and uses more tokens — don't waste it on early iterations.

When vibesmithy quality is good:
```bash
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/slugs \
  -d '{"slug":"docs-tune-1","content_type":"document","source_path":"/Users/adamlevine/AI Project Files/Core Selected Docs"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/docs-tune-1/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/docs-tune-1/build/question \
  -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'
```

## Key quality signals

| Signal | Good | Bad |
|--------|------|-----|
| L0 nodes touched | >70% of total | <30% |
| Depth distribution | 3-4 layers (L0→L1→L2→apex) | 1-2 layers |
| Apex content | Names specific systems, decisions, relationships | Vague overview or "this is a collection of files" |
| Branch count | Matches the major dimensions of the corpus | 1-2 mega-branches or 20+ micro-branches |
| KEEP/DISCONNECT ratio | More KEEP than DISCONNECT | Mostly DISCONNECT (pre-map noise) |
| MISSING count | Low (evidence base covers the questions) | High (questions asking for things L0 didn't extract) |
| Build time | <2 min for 34 files, <5 min for 127 | Mercury runaway (check for "length" finish reason in OpenRouter logs) |

## The research lab

`.lab/` has the experiment history from the previous tuning session. Key files:
- `.lab/results.tsv` — experiment results (baseline 1.50 → best 8.80)
- `.lab/log.md` — experiment log with insights
- `.lab/config.md` — rubric and metrics
- `.lab/friction-log.md` — issues that need Rust fixes (not prompt fixes)

You can use the `/researcher` skill to set up a fresh research series, or just iterate manually. The existing lab config and rubric are still valid.

## Constraints

- **No Rust changes.** The equipment is locked. Everything is prompt/YAML tuning.
- **No Pillar 37 violations.** No word counts, topic counts, sentence limits, length constraints in prompts. Describe goals, not dimensions.
- **Mercury 2 is the model.** Fast diffusion model. Sometimes generates exhaustively on unconstrained freeform fields. `/no_think` at the end of every prompt. Structured JSON output naturally bounds generation.
- **Archive test slugs when done.** DADBEAR watches active slugs and costs money on file changes.
