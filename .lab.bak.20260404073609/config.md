# Pyramid Quality Handoff — Research Lab

## Objective
Get document, code, and conversation pyramids building consistently with good structure: meaningful thread grouping, rich synthesis, proper convergence to apex, zero orphans, and no parse failures blocking builds.

## Metrics
- **Primary (qualitative):** Pyramid quality against the rubric below, using 3 blind evaluators per assessed artifact and the median composite score.

## Qualitative Rubric
- **Thread coherence** (weight 0.30): Do intermediate threads group genuinely related documents/files/conversation segments rather than mirroring the source one-to-one?
- **Apex quality** (weight 0.25): Does the apex orient a fresh reader to the material, its major domains, and why they matter?
- **Coverage / zero orphans** (weight 0.20): Are important source items represented without silent drops or unassigned leftovers?
- **Synthesis density** (weight 0.15): Do upper-layer nodes preserve decisions, relationships, specifics, and signal rather than thinning into vague enumeration?
- **Pipeline robustness** (weight 0.10): Does the build complete without parse failures or structural breakage that blocks understanding?

Scale: 1-10 per criterion, composite = weighted sum.

## Secondary Metrics
- Build completion: pass/fail
- Shape sanity from SQLite: depth counts
- Clustering quality: unique `source_node` count per thread from `batch_cluster` and merge outputs
- Orphan count: any source item not represented in the resulting thread structure
- Parse/heal failures: count and severity
- Build time: wall-clock seconds

## Scope
- `chains/defaults/*.yaml`
- `chains/prompts/**/*.md`
- `.lab/`

## Constraints
- No Rust changes
- Preserve unrelated working-tree changes
- Read and follow the Pillars and Patterns in `chains/CHAIN-DEVELOPER-GUIDE.md`
- Test on `vibesmithy` before larger code/document runs when possible

## Run Command
Use the local node/CLI to create ingests and build pyramids, then inspect the results through SQLite and pipeline-step JSON.

## Test Corpora
- Documents: `/Users/adamlevine/AI Project Files/Core Selected Docs/`
- Code (small): `/Users/adamlevine/AI Project Files/vibesmithy/`
- Code (large): current repo if needed
- Conversation: selected local conversation corpus or existing ingestable conversation source in this repo/runtime

## Wall-clock Budget
10 minutes per experiment

## Termination
Run until user interrupts, or until:
- Document builds reach roughly `L0:127 -> L1:15-30 -> L2:5-12 -> L3:1` with good grouping
- Code builds reach roughly `L0:34 -> L1:8-15 -> L2:3-6 -> L3:1` with zero orphans
- Conversation builds complete end-to-end with meaningful thread structure and apex

## Baseline
- Pending Experiment #0

## Best
- Pending baseline
