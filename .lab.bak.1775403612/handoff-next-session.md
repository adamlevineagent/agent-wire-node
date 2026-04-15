# Handoff: Question Pipeline v2 — Extract-First Tuning

## Status

The question pipeline has been architecturally redesigned from v1 (decompose → extract → evidence) to v2 (extract → web → decompose → evidence). The v2 YAML is live in `chains/defaults/question.yaml` (version 2.0.0). One new prompt created: `source_extract.md`. The implementation spec is at `docs/plans/question-pipeline-v2-extract-first.md` (builder-audited, all fixes applied).

## What's been done

### Experiments 0-6 (v1 pipeline prompt tuning)
- Utilization: solved (61.7% → 97-100%)
- Layer structure: solved (2 → 4 layers via branch guidance + conceptual framing)
- Empty nodes: NOT solved by prompts alone (28% in best v1 result, tune-6)
- Root cause identified: decompose runs before extraction, sees only `$characterize`, generates speculative questions about absent topics

### Architecture pivot to v2 (experiments 7-8)
- `source_extract.md` runs generic L0 extraction BEFORE decompose
- `l0_webbing` builds corpus structure map (currently without compact_inputs — was causing 47k length-outs)
- `refresh_state` (second cross_build_input) provides real L0 content to enhance + decompose
- `l2_webbing` added for branch-level cross-cutting connections
- evidence_loop + gap_processing reference `$refresh_state` not `$load_prior_state`

### v2 results (tune-8)
- 4 layers, 97% util, 22 questions (down from 47 in v1)
- BUT empty nodes went UP to 41% — new root cause: generic extraction mentions entities without containing their content, decompose sees mentions and asks about them
- source_extract.md was too detailed (CSS classes, prop values) — REWRITTEN to be tighter (role/purpose only, most sources = 1 topic)

### Rust fixes (all landed)
- `pyramid_file_hashes` WriteOp::UpdateFileHash for question pipeline
- Unified `## FILE:` chunk header format (was `## DOCUMENT:` for docs — silently broke file_hashes)
- Rate limiter (4 req/5s) with jittered backoff
- `llm_debug_logging` config flag
- Non-node progress tracking (steps count toward done/total)
- All friction log items resolved

## What needs doing next

### Immediate: Run tune-9 with tightened source_extract.md
The source_extract.md prompt was rewritten but NOT tested yet. Key change: "Most sources have one topic. Complex sources have two." + explicit "WHAT DOES NOT BELONG" section. This should dramatically reduce L0 extraction detail and prevent entity-mention-driven speculative questions.

```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
# Sync prompts first
SRC="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DST=~/Library/Application\ Support/wire-node/chains
cp "$SRC/defaults/question.yaml" "$DST/defaults/question.yaml"
for f in "$SRC/prompts/question/"*.md; do cp "$f" "$DST/prompts/question/$(basename "$f")"; done

# Fresh slug
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/slugs \
  -d '{"slug":"tune-9","content_type":"code","source_path":"/Users/adamlevine/AI Project Files/vibesmithy"}'
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/tune-9/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST \
  localhost:8765/pyramid/tune-9/build/question \
  -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'
```

### After tune-9: Run Haiku blind assessors
Same rubric as before (in the conversation history). Target: both PASS. Key metrics: empty node rate under 15%, apex names what system IS and DOES, depth reveals new info at each level.

### Open issues

1. **l0_webbing at scale**: compact_inputs was removed because it caused 47k length-outs (stripped too aggressively). For 127-doc corpora, the full L0 content (~100k+ tokens) may not fit in Mercury's 128k context. Need token-aware dehydration (the `dehydrate` cascade from document.yaml) instead of the blunt compact_inputs switch. This is a YAML config, not Rust.

2. **l0_webbing `when` guard**: Both source_extract and l0_webbing share `when: "$load_prior_state.l0_count == 0"`. If webbing fails but extraction succeeds, you can't re-run webbing without a fresh slug. Consider a separate guard (e.g., check if web edges exist).

3. **Decompose still produces entity-driven questions**: Even with real L0 content, if extractions mention "handoff guide" or "CLAUDE.md" as entities, decompose asks about them. source_extract.md tightening should help (fewer entities = fewer false mentions), but may need decompose.md to explicitly say "entities are references, not topics to ask about."

4. **UI doesn't trigger question builds**: The content type selector (Code/Documents/Conversation/Vine) triggers mechanical builds. Question builds require the API endpoint. Frontend needs a "Ask a question about this pyramid" flow.

5. **Scale test pending**: 127-doc Core Selected Docs pyramid hasn't been tested with v2 question pipeline. A mechanical document build was running during this session but question builds need the API.

6. **Delta build test**: No second-question delta build tested yet. The v2 pipeline's delta path (decompose_delta) should skip source_extract + l0_webbing and reuse existing L0. Untested.

## Key files

| File | What | Status |
|------|------|--------|
| `chains/defaults/question.yaml` | v2 pipeline YAML | Live, tested |
| `chains/prompts/question/source_extract.md` | Generic L0 extraction prompt | Rewritten, NOT tested |
| `chains/prompts/question/decompose.md` | Question decomposition | Cleaned up (removed compensatory sections) |
| `chains/prompts/question/extraction_schema.md` | Question-shaped extraction | Consolidated, terse |
| `chains/prompts/question/question_web.md` | Webbing prompt | Pillar 37 fixed |
| `docs/plans/question-pipeline-v2-extract-first.md` | Full implementation spec | Builder-audited, all fixes applied |
| `.lab/results.tsv` | Experiment tracking | Updated through tune-8 |
| `.lab/log.md` | Detailed experiment log | Updated through tune-8 |
| `.lab/friction-log.md` | Rust gaps | All resolved except #4 (decompose timing, now solved by v2) |

## Onboarding for next session

Read these in order:
1. `docs/chain-developer-guide-v2.md` — how the machinery works
2. `docs/question-pipeline-guide.md` — question pipeline specifics
3. `docs/plans/question-pipeline-v2-extract-first.md` — the v2 spec (builder-audited)
4. `chains/defaults/question.yaml` — the live v2 recipe
5. `.lab/log.md` — experiment history and root cause analysis
6. `.lab/results.tsv` — experiment scoreboard
