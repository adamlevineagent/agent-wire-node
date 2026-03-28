# Experiment Log

## Lab Initialized — research/chain-optimization
Branch: `research/chain-optimization` forked from `action-chain-refactor` @ f1316f9
Objective: Optimize code pyramid chain for reliability + marginal usefulness
Model: inception/mercury-2 (locked), qwen/qwen3.5-flash-02-23 for thread clustering

## Pre-experiments 0-3 — Infrastructure fixes
- Discovered chain YAML reads from data dir, not repo → added sync step to run script
- Fixed prompt refs ($prompts/ prefix) and for_each refs ($ prefix)
- Fixed model override passthrough in chain_dispatch.rs (Rust change)
- Added [CHAIN] logging for step-level visibility

## Experiment 0 — First successful build (baseline)
Branch: research/chain-optimization / Type: real / Parent: pre-3 / Commit: cc699f8
Metric: PASS / Secondary: usefulness ~30/100 / Status: keep / Duration: 475s
Result: 201 nodes, depth 0-5, 2 apex nodes (flush race condition), 8 failures. Used conversation distill prompt — apex was generic.
Insight: Pipeline works end-to-end but generic prompts produce generic output.

## Experiment 1 — Code-specific distill prompt
Branch: research/chain-optimization / Type: real / Parent: #0 / Commit: 95f64db
Metric: PASS / Secondary: usefulness ~55/100 / Status: keep / Duration: 590s
Result: 203 nodes, depth 0-5. Topics now specific (Backend Services, Feature Modules, UI Framework). Still 2 apex. 7 failures.
Insight: Code-specific prompts dramatically improve topic quality.

## Experiment 2 — Concise extract prompt
Branch: research/chain-optimization / Type: real / Parent: #1 / Commit: cd63312
Metric: PASS / Secondary: usefulness ~60/100 / Status: keep* / Duration: 373s
Result: 211 nodes, 3 failures (down from 7). Faster. Still 2 apex nodes.
Insight: Simpler extract prompt = fewer parse failures + faster. 3 remaining failures are >95k char files.

## Experiment 3 — Semantic grouping pipeline v2.0
Branch: research/chain-optimization / Type: real / Parent: #2 / Commit: d184909
Metric: PASS / Secondary: usefulness 77/100 / Status: keep / Duration: 869s / Slug: opt-test
Result: 125 nodes, 0 failures, single apex at L4. 7 L1 threads.
Blind testers (avg 77): Strong on naming(9.5), entities(9), headlines(8.5). Weak on build/deploy(5.5), data model(6.5), auth(7).
Insight: Semantic grouping >> blind 2:1 pairing. Thread clustering with qwen works well.

## Experiment 4 — Enriched extract + distill prompts
Branch: research/chain-optimization / Type: real / Parent: #3 / Commit: e0a4347
Metric: PASS / Secondary: usefulness 80/100 / Status: keep / Duration: 703s / Slug: opt-test
Result: 119 nodes, 2 failures, single apex L3. 5 threads.
Blind testers (avg 80): Data model(8), auth(8), entities(9.5). Build/deploy still 5/10.
Insight: Richer extract captures more, but only 5 threads = too few. Large subsystems crammed into single nodes.

## Experiment 5 — More threads (8-14), no junk drawers
Branch: research/chain-optimization / Type: real / Parent: #4 / Commit: 15a40b8
Metric: pending / Slug: opt-005
Hypothesis: More threads = better granularity. Ban "Utilities" catch-all. Enforce 5-20 files per thread.
Changes: code_cluster.md thread range 8-14, size balance rules, anti-catch-all rule
Running...
