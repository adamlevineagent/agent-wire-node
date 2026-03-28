# Research Configuration

## Objective
Optimize the code pyramid chain engine for two goals:
1. **Reliability** — builds complete successfully on first attempt, every time
2. **Marginal Usefulness** — a blind agent understands 80%+ of the codebase within 5 node visits

## Metrics

### Primary: Reliability (higher is better)
- **Measure**: Create slug → ingest → build → apex returns valid content
- **Pass/Fail**: Binary. Either the pyramid completes with a valid apex or it doesn't.
- **Target**: 100% (3 consecutive first-try successes)

### Secondary: Marginal Usefulness (higher is better)
- **Measure**: Qualitative composite — blind agent reads apex + up to 4 drills, answers 10 questions
- **Scoring**: 0-10 per question, composite = sum (max 100)
- **Target**: 80/100

### Usefulness Rubric
| Criterion | Weight | What it measures |
|-----------|--------|-----------------|
| Apex orientation | 0.25 | Can the agent describe what the project is/does from apex alone? |
| Drill efficiency | 0.20 | Does the agent find the right node in ≤3 drills? |
| Coverage | 0.25 | Are key systems (pyramid, chain engine, partner AI, vine, MCP) represented? |
| Distortion | 0.15 | Are claims in nodes factually correct? |
| Thread coherence | 0.15 | Is each L2 thread about ONE clear topic? |

## Chain Under Test
`chains/defaults/code.yaml` — code pipeline with concurrent processing

## Source Material
The `agent-wire-node` folder itself (self-referential pyramid)

## Models
- **Default**: `inception/mercury-2` (locked — all steps except thread clustering)
- **Thread clustering (step 5)**: `qwen/qwen3.5-flash-02-23` (1M context window, needed for large topic inventories)

## Scope
- Prompt files: `chains/prompts/code/*.md`
- Chain YAML: `chains/defaults/code.yaml`
- Rust source: only if a reliability bug is found in chain_executor.rs / chain_dispatch.rs

## Constraints
- Model selection is fixed (mercury-2)
- No conversation prompts — code pipeline only
- Rust changes require clear justification (reliability bug)

## Run Command
```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
BASE="http://localhost:8765/pyramid"

# 1. Delete old test slug (ignore 404)
curl -s -H "$AUTH" -X DELETE "$BASE/opt-test"

# 2. Create slug
curl -s -H "$AUTH" -H "Content-Type: application/json" \
  -X POST "$BASE/slugs" \
  -d '{"slug":"opt-test","content_type":"code","source_path":["/Users/adamlevine/AI Project Files/agent-wire-node"]}'

# 3. Ingest
curl -s -H "$AUTH" -X POST "$BASE/opt-test/ingest"

# 4. Build
curl -s -H "$AUTH" -X POST "$BASE/opt-test/build"

# 5. Poll status until complete or failed (timeout 10min)
# 6. Check apex
node "/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js" apex opt-test
```

## Wall-Clock Budget
10 minutes per experiment

## Termination Condition
- 3 consecutive successful first-try builds AND usefulness score ≥ 80/100
- OR user interrupts

## Baseline
Pending — chain engine not yet enabled

## Best
Pending
