# Research Configuration

## Objective
Optimize the document pyramid chain prompts for intelligence-driven behavior. Key principles:
1. Don't prescribe counts or ranges — describe purpose, let the model decide
2. Separate thinking from formatting — intelligence shouldn't do bookkeeping
3. Allow orphans — don't force irrelevant docs into threads
4. Handle large corpora (100+ docs) without monolithic calls
5. The prompts should describe WHAT we want and WHY, not HOW MANY

## Metrics

### Primary: Build Completion Time (lower is better)
- **Measure**: Wall-clock time from build start to completion
- **Baseline**: ~15+ minutes for clustering step alone on 127 docs (Grok)
- **Target**: Full build completes in <5 minutes

### Secondary: Pyramid Quality (higher is better)
- **Measure**: Qualitative composite — blind agent scores via rubric
- **Scoring**: 6 dimensions (1-10 each), composite = average
- **Target**: 6.5/10 composite (up from 4.2 baseline)

### Quality Rubric
| Criterion | Weight | Description |
|-----------|--------|-------------|
| Directness | 0.17 | Does the apex answer the seed question head-on? |
| Audience Match | 0.17 | Appropriate for the specified audience? |
| Evidence Grounding | 0.17 | Claims traceable to L0 source material? |
| Completeness | 0.17 | Important aspects covered, no major gaps? |
| Coherence | 0.17 | Logical flow from L0 → L1 → apex? |
| Conciseness | 0.17 | Efficient, no repetition or filler? |

## Chain Under Test
`chains/defaults/document.yaml` — document pipeline with four-axis semantic grouping

## Source Material
`/Users/adamlevine/AI Project Files/Core Selected Docs` — 127 design docs across 3 projects

## Models
- **Default**: `inception/mercury-2` (primary for extraction, synthesis)
- **Large context**: `qwen/qwen3.5-flash-02-23` (classification, clustering — needs full corpus)
- **Frontier**: `x-ai/grok-4.20-beta` (decomposition only)

## Scope
- Prompt files: `chains/prompts/document/*.md`
- Chain YAML: `chains/defaults/document.yaml`
- Rust source: only if a reliability bug blocks testing

## Constraints
- DO NOT change max_tokens — fix the prompt, not the ceiling
- DO NOT prescribe counts or ranges in prompts
- Preserve the four-axis classification system (temporal, conceptual, canonical, type)
- All changes must be backward-compatible with existing pyramids

## Run Command
```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
BASE="http://localhost:8765/pyramid"

# Create fresh slug
curl -s -H "$AUTH" -X DELETE "$BASE/doc-opt-test" 2>/dev/null
curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST "$BASE/slugs" \
  -d '{"slug":"doc-opt-test","content_type":"document","source_path":"/Users/adamlevine/AI Project Files/Core Selected Docs"}'

# Ingest
curl -s -H "$AUTH" -X POST "$BASE/doc-opt-test/ingest"

# Build
curl -s -H "$AUTH" -X POST "$BASE/doc-opt-test/build"

# Poll until complete (timeout 15min)
for i in $(seq 1 90); do
  sleep 10
  STATUS=$(curl -s -H "$AUTH" "$BASE/doc-opt-test/build/status")
  echo "$STATUS"
  echo "$STATUS" | grep -q '"complete"' && break
  echo "$STATUS" | grep -q '"failed"' && break
done

# Check apex
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" apex doc-opt-test
```

## Wall-Clock Budget
15 minutes per experiment

## Termination Condition
- Build completes in <5 minutes AND quality ≥ 6.5/10
- OR user interrupts

## Baseline
Pending — first experiment will establish

## Best
Pending
