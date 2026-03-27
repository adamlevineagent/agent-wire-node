# Experiment Log — Document Prompt Optimization

## Lab Initialized
Branch: `research/chain-optimization` @ c0ee715
Objective: Make document chain prompts intelligence-driven instead of prescriptive
Focus: `chains/prompts/document/*.md` + `chains/defaults/document.yaml`
Corpus: Core Selected Docs (127 documents across 3 projects)

## Experiment 1 — Intelligence-driven clustering (no prescribed counts)
Branch: research/chain-optimization / Type: real / Parent: baseline / Commit: 880ec5b
Hypothesis: Removing prescribed thread counts (6-14), max thread size (15), and zero-orphans rule will let the model produce natural groupings faster and with better coverage. The model will no longer waste output tokens on bookkeeping fields (doc_type, date, canonical in assignments) that are already in the classification.
Changes:
- doc_cluster.md: purpose+principles instead of rules+counts, unassigned array
- doc_recluster.md: removed prescribed 3-5 cluster count
- document.yaml: schema updated to match
Status: COMPLETE — mechanical build ran in 810s (13.5 min), 138 nodes, 89 L0 → 8 L1 → 4 L2 (no single apex — recluster didn't converge).

**Observation:** Question overlay build HUNG — binary doesn't have latest Rust fixes. Need a rebuild for overlay testing. Pivoting to mechanical prompt optimization which doesn't need rebuilds.

## Experiment 1b — Mechanical build with intelligence-driven clustering
Branch: research/chain-optimization / Type: real / Commit: 880ec5b
Testing the same prompt changes on a fresh mechanical build to measure clustering quality + speed.
Note: The prompt changes were synced to the data dir; the YAML schema change (adding `unassigned`) was in the last build.
