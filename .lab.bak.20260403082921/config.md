# Vibesmithy Code Pyramid Quality — Research Lab

## Objective
Improve the YAML-driven code pyramid pipeline so builds over the `vibesmithy` folder produce materially useful understanding pyramids, with a strong apex and natural intermediate threads.

This is a fresh series. Prior `.lab` findings may be mined for ideas, but they are not treated as valid evidence because the system has been substantially rebuilt.

## Metrics
- **Primary (qualitative):** Code pyramid usefulness, evaluated by 3 blind evaluators against the rubric below
- **Rubric:**
  - **Apex quality** (weight 0.30): Does the apex orient a fresh reader to what `vibesmithy` is, how it is organized, and why the parts matter?
  - **Thread coherence** (weight 0.25): Do L1 threads describe real architectural domains rather than arbitrary bundles of files?
  - **Coverage** (weight 0.20): Are the important systems, flows, and responsibilities represented?
  - **Drill usefulness** (weight 0.15): Would an agent be able to drill from apex to the right thread/file-level nodes to answer concrete questions?
  - **Specificity / low distortion** (weight 0.10): Are claims concrete and accurate rather than generic or misleading?
- Scale: 1-10 per criterion, composite = weighted sum

## Secondary Metrics
- Build completion: pass/fail
- Apex presence: single apex node present / absent
- Node shape: depth counts from SQLite for sanity

## Test Corpus
- **Primary:** `vibesmithy` folder, using the code pipeline

## Scope
- `chains/defaults/code.yaml`
- `chains/prompts/code/**/*.md`
- `.lab/`

## Constraints
- No Rust changes
- Do not rely on old experiment conclusions without re-testing on the rebuilt system
- Preserve unrelated working-tree changes outside research edits

## Run Command
Build the `vibesmithy` code pyramid through the local node/API, then inspect the apex and node structure from the database/CLI.

## Wall-clock Budget
10 minutes per experiment

## Termination
Run until user interrupts or primary metric >= 8.5

## Baseline
- Experiment #0 (`vibesmithy5` existing build): provisional composite 3.9 / 10
- Shape sanity: 34 L0 -> 34 L1 -> 1 L2
- Key failure mode: every L1 node is still a single-file node, so the pyramid reaches an apex without discovering architectural threads

## Best
- Current best: Experiment #1, provisional composite 5.1 / 10
