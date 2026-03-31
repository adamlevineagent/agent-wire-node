# Web Research Lab Config

## Objective
Score 10/10 on the understanding web quality rubric. A smart high school graduate reads the apex and IMMEDIATELY gets what this is, why they care, and wants to show friends.

## Primary Metric
Composite score (average of 6 dimensions, each 1-10): Directness, Audience Match, Evidence Grounding, Completeness, Coherence, Conciseness

## Direction
Higher is better

## Baseline
6.3/10 on vibe-web-2

## Score History
- 4.2 — first evidence pipeline (ev7)
- 5.5 — audience framing + L0 dedup (ev8)
- 6.3 — cross-slug architecture + human-interest prompts (web-2)

## Known Weaknesses
1. L3 decomposition produces overlapping questions → near-identical answers at every layer
2. "Why should I care" is generic — no concrete scenarios, emotional hooks
3. "colorful bubbles + AI helper + pyramids" repeats at EVERY layer
4. L0 nodes still lean technical ("This file builds..." / "This file defines...")
5. Missing: what makes it different, how data gets in, what using it feels like

## Scope (editable prompt files)
All at `~/Library/Application Support/wire-node/chains/prompts/`:
- `question/enhance_question.md`
- `question/decompose.md`
- `question/horizontal_review.md`
- `code/code_extract.md`
- `code/code_distill.md`

## Constraints
- Do NOT modify Rust source code
- L0 nodes come from vibe-ev8 base pyramid (can't rebuild without code_extract changes + full rebuild)
- Pillar 37: never prescribe output structure to the LLM

## Run Command
```bash
# Create slug
curl -s -X POST -H "Authorization: Bearer vibesmithy-test-token" -H "Content-Type: application/json" \
  http://localhost:8765/pyramid/slugs \
  -d '{"slug":"exp-N","content_type":"question","referenced_slugs":["vibe-ev8"]}'

# Build
curl -s -X POST -H "Authorization: Bearer vibesmithy-test-token" -H "Content-Type: application/json" \
  http://localhost:8765/pyramid/exp-N/build/question \
  -d '{"question":"What is this and why do I care? Questioner is a smart high school graduate, not a developer.","granularity":3,"max_depth":5}'

# Poll until done
curl -s -H "Authorization: Bearer vibesmithy-test-token" http://localhost:8765/pyramid/exp-N/build/status
```

## Wall-Clock Budget
5 minutes per build

## Termination
Score >= 9.0 or user interrupts

## Best
TBD
