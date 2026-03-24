# Chain Engine Optimization Guide

## For /researcher agents

---

## Two Metrics

Everything collapses into two numbers:

### 1. Reliability (target: 100%)

Does the build complete successfully, every time, first attempt? Binary. Either the pyramid finishes with a valid apex or it doesn't. No retries, no manual intervention, no "rebuild 2-3 times to get L5."

### 2. Marginal Usefulness (target: 50+ points)

Given the pyramid, how much more useful is an agent than one without it?

**Measurement:** Give an agent a task about the source material. Run it twice — once with the pyramid, once without. The delta in task completion rate is the marginal usefulness.

- Agent without pyramid completes 30% of tasks correctly
- Agent with pyramid completes 80% of tasks correctly
- Marginal usefulness = 50 points

Everything else (apex quality, drill efficiency, coverage, distortion, thread coherence) is a sub-metric of marginal usefulness. A pyramid with perfect coverage but terrible navigation has low marginal usefulness. A pyramid with great navigation but missing facts also has low marginal usefulness.

**The researcher optimizes: reliability to 100%, then maximizes marginal usefulness.**

---

## What Drives Marginal Usefulness

These are the sub-metrics. Improving any of them improves marginal usefulness:

| Sub-metric | What it means | How to measure |
|------------|--------------|----------------|
| **Apex quality** | Fresh agent reads apex, accurately describes the project | Score 0-10 against ground truth |
| **Drill efficiency** | Drills from apex to answer a specific question | Count drills, target median ≤ 3 |
| **Coverage** | Important facts/decisions appear in the pyramid | List 10 known facts, search for each |
| **Distortion** | Pyramid never says anything wrong | Spot-check 10 claims against source |
| **Thread coherence** | Each L2 thread is about ONE thing | Rate headlines 1-5, target mean ≥ 4 |

---

## System Overview

The chain engine takes YAML chain definitions + markdown prompt files and executes them as pyramid build pipelines. The Rust runtime handles sequencing, resume, error handling, and DB persistence.

**Feature flag:** `use_chain_engine` in `pyramid_config.json`
- `false` (default) = legacy hardcoded build.rs pipelines
- `true` = chain engine reads YAML, executes steps

**Config location:** `/Users/adamlevine/Library/Application Support/wire-node/pyramid_config.json`

**DB location:** `/Users/adamlevine/Library/Application Support/wire-node/pyramid.db`

---

## The Levers

### Prompt files (biggest impact on marginal usefulness)

All at `chains/prompts/`:

| File | What it controls | Sub-metrics affected |
|------|-----------------|---------------------|
| `conversation/forward.md` | What gets extracted from each chunk | Coverage, Distortion |
| `conversation/reverse.md` | What gets marked as survived/superseded | Coverage, Distortion |
| `conversation/combine.md` | How forward+reverse merge into L0 | Coverage, Distortion |
| `conversation/distill.md` | How sibling nodes synthesize (used at L1 and L3+) | Apex Quality, Drill Efficiency |
| `conversation/thread_cluster.md` | How topics group into threads | Thread Coherence, Drill Efficiency |
| `conversation/thread_narrative.md` | How threads are narrated | Apex Quality, Coverage |

### Chain YAML (structural impact on reliability + usefulness)

At `chains/defaults/conversation.yaml`:

| Setting | Effect |
|---------|--------|
| `on_error: retry(N)` | Reliability — more retries = more resilient |
| `batch_threshold: 30000` | Thread clustering — lower = more batches |
| `accumulate.max_chars: 1500` | Running context window — more = better continuity, higher cost |
| `temperature: 0.3` | Determinism — lower = more consistent |
| Step ordering | Removing reverse pass saves ~33% cost but may reduce coverage |

### Model selection (cost/quality tradeoff)

Set per-step in chain YAML using OpenRouter model slugs from https://openrouter.ai/models:

```yaml
steps:
  - name: "forward_pass"
    model: "inception/mercury-2"           # fast, cheap extraction
  - name: "thread_clustering"
    model: "qwen/qwen3.5-flash-02-23"     # better reasoning for clustering
  - name: "upper_layers"
    model: "x-ai/grok-4.20-beta"          # strongest for apex synthesis
```

---

## How to Test

### Enable chain engine

```bash
CONFIG="/Users/adamlevine/Library/Application Support/wire-node/pyramid_config.json"
python3 -c "
import json
with open('$CONFIG') as f: c = json.load(f)
c['use_chain_engine'] = True
with open('$CONFIG', 'w') as f: json.dump(c, f, indent=2)
print('Chain engine enabled — restart app')
"
```

### Build a test pyramid

```bash
CLI="/Users/adamlevine/AI Project Files/agent-wire-node/mcp-server/dist/cli.js"
TEST_DIR="/Users/adamlevine/.claude/projects/-Users-adamlevine-AI-Project-Files/"

# Create + ingest + build
curl -s -H "Authorization: Bearer vibesmithy-test-token" \
  -H "Content-Type: application/json" \
  -X POST "http://localhost:8765/pyramid/slugs" \
  -d "{\"slug\":\"opt-test\",\"content_type\":\"conversation\",\"source_path\":\"[\\\"$TEST_DIR\\\"]\"}"

curl -s -H "Authorization: Bearer vibesmithy-test-token" \
  -X POST "http://localhost:8765/pyramid/opt-test/ingest"

curl -s -H "Authorization: Bearer vibesmithy-test-token" \
  -X POST "http://localhost:8765/pyramid/opt-test/build"

# Monitor
watch -n 5 'curl -s -H "Authorization: Bearer vibesmithy-test-token" \
  http://localhost:8765/pyramid/opt-test/build/status'
```

### Measure reliability

```bash
# Did it complete?
node "$CLI" apex opt-test

# Check structure
DB="/Users/adamlevine/Library/Application Support/wire-node/pyramid.db"
sqlite3 "$DB" "SELECT depth, COUNT(*) FROM pyramid_nodes WHERE slug='opt-test' GROUP BY depth ORDER BY depth;"
```

### Measure marginal usefulness

```bash
# Read the apex
node "$CLI" apex opt-test | python3 -c "import sys,json; print(json.load(sys.stdin)['distilled'])"

# Prepare 10 test questions about the source material
# For each question:
#   1. Ask a fresh agent WITHOUT the pyramid → record answer quality (0-10)
#   2. Ask a fresh agent WITH the pyramid (apex + 2-3 drills) → record answer quality (0-10)
#   3. Delta = marginal usefulness for that question

# Example questions for the agent-wire-node codebase:
# - What is DADBEAR and how does it work?
# - How does the pyramid build pipeline handle errors?
# - What database does the system use?
# - How does the partner (Dennis) AI system work?
# - What is the vine conversation system?
```

### Iterate

1. Make a change (edit a prompt, tweak a YAML setting, try a different model)
2. Delete the test slug: `curl -s -H "..." -X DELETE http://localhost:8765/pyramid/opt-test`
3. Rebuild
4. Re-measure
5. Record results

---

## Key Files

| File | Purpose |
|------|---------|
| `src-tauri/src/pyramid/chain_executor.rs` | Main execution loop (reliability) |
| `src-tauri/src/pyramid/chain_dispatch.rs` | LLM dispatch + JSON retry (reliability) |
| `chains/defaults/conversation.yaml` | Pipeline structure (both metrics) |
| `chains/prompts/**/*.md` | Prompt templates (marginal usefulness) |
| `docs/architecture/action-chain-system.md` | Full design reference |

---

## North Star

**Reliability: 100%. Marginal usefulness: 50+ points.**

An agent with the pyramid should be dramatically more effective than one without it. If the pyramid doesn't meaningfully change outcomes, it's not earning its cost.
