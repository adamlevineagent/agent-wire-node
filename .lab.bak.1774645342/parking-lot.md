# Parking Lot — Deferred Ideas

## HIGH PRIORITY — Rust Changes Needed
- [ ] **Children ID normalization**: LLM returns unpadded IDs (C-L0-70) but nodes use zero-padded (C-L0-070). Fix in children wiring: normalize `C-L0-\d+` → 3-digit padding. Affects L1-009 and several other L1 nodes in opt-010.
- [ ] **Carry-left orphan problem**: chain_executor.rs line 889 — odd nodes get promoted verbatim, creating useless duplicate nodes all the way to apex. Fix: do 3-to-1 merge when odd, or merge last 3 instead of last-pair-plus-carry. This is the #1 structural quality issue.
- [ ] **Layer-by-layer rebuild**: Allow rebuilding L1+ without re-running L0 extraction. L0s are stable and expensive (112 chunks × mercury-2). Only re-run the clustering + synthesis.
- [ ] **Concurrency for L0**: Currently sequential (~8min for 112 files). Legacy had 3x concurrent. Chain executor should support `concurrency: N` in YAML.

## Prompt Optimization
- [x] code_extract.md: Enriched with data_model, auth_security, deployment_ops, module_relationships fields
- [x] code_cluster.md: 8 threads for <150 files, no junk drawers, size balance
- [ ] code_thread.md: Add "preserve ALL entity names" emphasis — threads lose specifics
- [ ] code_distill.md: Could preserve entity counts as a quality signal ("merging 45 entities from child A + 32 from child B")
- [ ] config_extract.md: Still very minimal — test enriching with env vars, scripts, notable settings

## Chain YAML
- [ ] Temperature 0.3 → test 0.1 for more deterministic clustering
- [ ] Batch threshold for thread clustering — currently no batching since qwen has 1M context
- [ ] Error strategy: retry(2) for L0 — test retry(3) for large files that occasionally timeout

## Quality Experiments
- [ ] Test blind testers with specific codebase questions (e.g., "find the auth flow", "how does the build pipeline work")
- [ ] Compare pyramid navigation efficiency: how many drills to answer each question
- [ ] Test with a fresh codebase (not self-referential) to avoid overfitting
