# Lab Config — Question Pyramid Prompt Tuning

## Objective
Tune the question pipeline prompts to maximize evidence utilization and answer quality on vibesmithy corpus (34 files, ~70s builds). Two phases:
- **Phase A (current):** Get evidence utilization to ~100% — every L0 node connected to at least one question answer. Primary metric: utilization %.
- **Phase B (after Phase A target hit):** Qualitative — two Haiku blind testers, rubric focused on "apex + all sub-questions answered for maximal usefulness."

## Phase A Primary Metric
**Evidence utilization %** — fraction of L0 nodes (depth=0) for the test slug that have at least one KEEP verdict. Higher is better. Target: >95%.

Measure command:
```bash
DB=~/Library/Application\ Support/wire-node/pyramid.db
SLUG="tune-N"
TOTAL=$(sqlite3 "$DB" "SELECT COUNT(*) FROM pyramid_nodes WHERE slug='$SLUG' AND depth=0;")
TOUCHED=$(sqlite3 "$DB" "SELECT COUNT(DISTINCT source_node_id) FROM pyramid_evidence pe JOIN pyramid_nodes pn ON pe.source_node_id=pn.id WHERE pn.slug='$SLUG' AND pn.depth=0 AND pe.verdict='KEEP';")
echo "$TOUCHED / $TOTAL = $(echo "scale=1; $TOUCHED * 100 / $TOTAL" | bc)%"
```

## Secondary Metrics
- Layer count: `SELECT MAX(depth) FROM pyramid_nodes WHERE slug='$SLUG';` — target 3-4
- KEEP count: total KEEP verdicts on L0 nodes
- MISSING count: total MISSING verdicts (lower = better)
- Apex content: read distilled from max-depth node

## Run Command
```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
SLUG="tune-N"

curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/slugs \
  -d "{\"slug\":\"$SLUG\",\"content_type\":\"code\",\"source_path\":\"/Users/adamlevine/AI Project Files/vibesmithy\"}"
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/$SLUG/ingest

SRC="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DST=~/Library/Application\ Support/wire-node/chains
cp "$SRC/defaults/question.yaml" "$DST/defaults/question.yaml"
for f in "$SRC/prompts/question/"*.md; do cp "$f" "$DST/prompts/question/$(basename "$f")"; done

curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/$SLUG/build/question \
  -d '{"question":"What is this body of knowledge and how is it organized?","granularity":3,"max_depth":3}'
```

## Scope
- chains/prompts/question/*.md
- chains/defaults/question.yaml
- NO Rust changes

## Constraints
- No Pillar 37 violations (no numbers constraining LLM output)
- /no_think at end of every prompt
- JSON output format instructions in every prompt
- Mercury 2 model
- Archive test slugs when done

## Wall-Clock Budget Per Experiment
5 minutes

## Termination
Infinite — until user interrupts or >95% utilization sustained on 2 consecutive experiments (then switch to Phase B).

## Baseline
- Experiment #0: TBD (run first build with no changes)
- Best so far: TBD
