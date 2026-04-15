# Pyramid Build Quality — Research Lab

## Objective
Improve bottom-up mechanical pyramid creation to produce maximally useful *understanding* pyramids.

### Priority 1: Apex production
Pyramids must produce a single apex node. Root cause: IR executor's static convergence loop had a gap where 2-4 nodes at top layer triggered no apex synthesis. Fix: switched to chain executor (`use_ir_executor: false`) which uses a dynamic loop. **Validate this works.**

### Priority 2: Understanding over mechanics
The recluster/distill prompts prescribe too much (cluster counts, reduction targets) instead of letting LLM intelligence determine natural structure. The apex should be genuine understanding, not a forced merge. Intermediate layers should represent natural dimensions of understanding — what someone needs to grasp.

### Priority 3: Conversation pyramids
Get forward→reverse→combine→cluster→synthesize→apex working end-to-end in the chain engine path.

## Metrics
- **Primary (qualitative):** Pyramid understanding quality — evaluated by 3 blind agents against rubric
- **Rubric:**
  - **Apex coherence** (weight 0.30): Does the apex give genuine understanding of the whole? Could a newcomer read it and orient?
  - **Layer meaningfulness** (weight 0.25): Do intermediate layers represent natural dimensions of understanding, not mechanical groupings?
  - **Information density** (weight 0.20): Are nodes dense with specifics (names, functions, relationships) vs generic filler?
  - **Structural naturalness** (weight 0.15): Does the pyramid shape feel right? Not too deep/shallow? Layers correspond to how you'd explain it?
  - **Completeness** (weight 0.10): Is important material represented? Nothing major dropped?
- Scale: 1-10 per criterion, composite = weighted sum

## Test Corpus
- **Primary:** vibesmithy (34 L0 nodes, code pipeline — fast iteration)
- **Secondary:** core-selected-docs (127 L0, document pipeline — validates at scale)

## Scope
- `chains/prompts/**/*.md` — prompt files
- `chains/defaults/*.yaml` — chain definitions
- `.lab/` — experiment workspace
- **OFF LIMITS:** `src-tauri/**/*.rs` — no Rust changes

## Run Command
Trigger build via app UI or HTTP API on vibesmithy slug, then query results from SQLite.

## Wall-clock Budget
10 minutes per experiment (vibesmithy builds should complete in 2-5 min)

## Termination
Run until user interrupts or primary metric ≥ 8.5

## Baseline
TBD — experiment #0 after app restart confirms apex production

## Best
TBD
