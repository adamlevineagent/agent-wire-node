# Lab Config — Lens Framework Research

## Objective
Find a generalized prompt framework for the question pyramid pipeline that produces maximally marginally useful pyramids across any corpus type (code, docs, or mixed). The current 4-lens framework (Value/Intent, Kinetic/State Flow, Temporal, Metaphorical) is a Pillar 37 violation — it prescribes exactly 4 axes regardless of what the corpus naturally yields.

The pyramid is a scaffold, not a final product. It needs to be coherent and useful enough that the system can detect unanswered questions and build additions. Agents annotate it, the FAQ system creates meta-knowledge on top.

Default altitude should be ground truth, not developer truth. "What IS this?" — accessible to someone who doesn't know the domain. Technical depth comes on-demand via follow-up questions.

## Primary Metric
**Maximal Marginal Usefulness (MMU)** — qualitative, agent-judged via Multi-Evaluator Protocol.

Each layer should maximize the NEW understanding it adds beyond what the layer below already provides:
- L0: ground truth extracted from sources
- L1: synthesizes L0s — value is only what you learn that you couldn't from reading the L0s individually
- L2: cross-cutting insights invisible at L1 altitude
- Apex: thesis about the whole system that no individual branch could give you

A pyramid where each layer merely restates the layer below in fewer words has near-zero MMU. A pyramid where each layer genuinely surprises you with emergent insight has high MMU.

### Rubric (v1) — SUPERSEDED by v2
Scale: 1-10 per criterion. Composite = weighted sum.

| Criterion | Weight | What 1 looks like | What 10 looks like |
|-----------|--------|-------------------|-------------------|
| Layer Lift | 0.35 | Each layer just paraphrases the one below | Each layer reveals something genuinely new that only becomes visible at that altitude |
| Scaffold Quality | 0.25 | Structure doesn't suggest what questions to ask next | Reading any node immediately suggests what's missing and what to investigate |
| Corpus Fidelity | 0.20 | Structure imposed regardless of material; could be any corpus | Structure clearly emerged from THIS corpus; couldn't have been predicted in advance |
| Accessibility | 0.20 | Assumes domain expertise; jargon-heavy; only useful to insiders | A smart non-expert can orient themselves; technical depth available but not default |

### Rubric (v2) — Agent Utility
Revised after first-contact testing by two independent agents (Antigravity-A/B). v1 penalized structural noise (duplicate L1 questions) that is actually a horizontal_review bug, and missed what matters: can an agent cold-start on this pyramid and build useful understanding?

Scale: 1-10 per criterion. Composite = weighted sum.

| Criterion | Weight | What 1 looks like | What 10 looks like |
|-----------|--------|-------------------|-------------------|
| Cold-Start Orientation | 0.30 | Reading the apex + one L2 drill tells you nothing useful about the corpus | Apex + one drill gives you enough to understand what this is and where to go next |
| Concept Discovery | 0.30 | No domain-specific concepts surfaced; generic summaries only | Pyramid surfaces specific named concepts, mechanisms, and vocabulary you wouldn't know to search for |
| Navigable Depth | 0.20 | Drilling down just reveals more detail about the same thing | Each layer down reveals meaningfully DIFFERENT information — new mechanisms, trade-offs, specifics |
| Growth Scaffold | 0.20 | Structure is a dead end; no gaps, no suggested investigations | Gaps and question structure reveal what's missing and suggest productive next investigations |

**Why this is better than v1:**
- "Cold-Start Orientation" replaces "Accessibility" — tests whether an agent can actually USE the pyramid, not just whether it avoids jargon
- "Concept Discovery" replaces "Corpus Fidelity" — measures whether the pyramid found River-Graph Boundary, Progressive Crystallization, etc., not whether the L2 branch labels are generic
- "Navigable Depth" replaces "Layer Lift" — measures the agent's drill experience, not structural elegance
- "Growth Scaffold" replaces "Scaffold Quality" — explicitly measures gap identification and question-generation, the pyramid's actual purpose as a scaffold

### Evaluator Instructions
Each evaluator subagent receives:
- The full pyramid (apex + all L2/L1 nodes with distilled text and self_prompts)
- A sample of L0 nodes (5-8 representative ones)
- The rubric (v2)
- No context about what experiment this is or what changed

## Secondary Metrics
- Layer count: target 3-4
- Empty node %: nodes with no KEEP evidence (lower is better)
- Build duration (seconds)
- L0 count

## Test Corpus
Architecture docs from Core Selected Docs: `/Users/adamlevine/AI Project Files/Core Selected Docs/architecture/`
34 documents, content_type: document

## Run Command
```bash
AUTH="Authorization: Bearer vibesmithy-test-token"
SLUG="lens-N"

# Sync prompts
SRC="/Users/adamlevine/AI Project Files/agent-wire-node/chains"
DST="$HOME/Library/Application Support/wire-node/chains"
for f in "$SRC/prompts/question/"*.md; do cp "$f" "$DST/prompts/question/$(basename "$f")"; done

# Create, ingest, build
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/slugs \
  -d "{\"slug\":\"$SLUG\",\"content_type\":\"document\",\"source_path\":\"/Users/adamlevine/AI Project Files/Core Selected Docs/architecture\"}"
curl -s -H "$AUTH" -X POST localhost:8765/pyramid/$SLUG/ingest
curl -s -H "$AUTH" -H "Content-Type: application/json" -X POST localhost:8765/pyramid/$SLUG/build/question \
  -d '{"question":"What is this?","granularity":3,"max_depth":3}'
```

## Scope
- chains/prompts/question/*.md (all 17 prompt files)
- NO Rust changes
- NO chain YAML structure changes (step order, primitives, etc.)

## Constraints
- No Pillar 37 violations (no numbers constraining LLM output)
- /no_think at end of every prompt
- JSON output format in every prompt
- Model: minimax/minimax-m2.7 (set in UI settings, not in prompts)
- Follow Chain Developer Guide rules

## Wall-Clock Budget Per Experiment
5 minutes

## Termination
Infinite — run until user interrupts or we find a framework that scores 8+ MMU sustained across 2+ consecutive experiments.

## Baseline
- Experiment #0: TBD
- Best so far: TBD
